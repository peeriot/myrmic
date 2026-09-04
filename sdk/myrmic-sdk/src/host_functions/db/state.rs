//! A typed handle to a single value stored under a fixed key.

use alloc::string::String;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};

use myrmic_common::db::Scope;

use crate::db::{key_get, key_put};
use crate::{Codec, Postcard, Result};

/// A typed handle to a single value stored under `key`.
///
/// Reads and writes go through the codec `C`, so callers work with `T`
/// directly rather than raw bytes. Use [`State::new`] for a runtime key or
/// [`State::new_const`] to declare one at compile time.
pub struct State<'a, T, C = Postcard> {
    key: &'a str,
    scope: Scope,
    _marker: PhantomData<(T, C)>,
}

impl<'k, V, C> State<'k, V, C> {
    /// Declare a handle at compile time in the default scope.
    pub const fn new_const(key: &'static str) -> Self {
        Self::new_const_in(key, Scope::private())
    }

    /// Declare a handle at compile time in `scope`.
    pub const fn new_const_in(key: &'static str, scope: Scope) -> Self {
        Self {
            key,
            scope,
            _marker: PhantomData,
        }
    }

    /// Create a handle in the default scope.
    pub fn new(key: &'k str) -> Self {
        Self::new_in(key, Scope::default())
    }

    /// Create a handle in `scope`.
    pub fn new_in(key: &'k str, scope: Scope) -> Self {
        Self {
            key,
            scope,
            _marker: Default::default(),
        }
    }

    fn scope(&self) -> Scope {
        self.scope.clone()
    }
}

impl<'k, T, C> State<'k, T, C>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
    C: Codec,
{
    /// Load the value, or `None` if nothing is stored.
    pub fn load(&self) -> Result<Option<T>> {
        self.load_from(self.key)
    }

    /// Load from an explicit `key`, or `None` if nothing is stored.
    pub fn load_from(&self, key: &str) -> Result<Option<T>> {
        let key = String::from(key);

        let mut req = [0u8; 256];
        let mut resp = alloc::vec![0u8; 8192];

        key_get(self.scope(), key, &mut req, &mut resp)?
            .map(|b| C::decode(&b))
            .transpose()
    }

    /// Store `val`, overwriting any existing value.
    pub fn save(&self, val: &T) -> Result {
        self.save_to(self.key, val)
    }

    /// Store `val` under an explicit `key`, overwriting any existing value.
    pub fn save_to(&self, key: &str, val: &T) -> Result {
        let key = String::from(key);

        let bytes = C::encode(val)?;
        let mut buf = alloc::vec![0u8; bytes.len() + 1024];
        key_put(self.scope(), key, bytes, &mut buf).map_err(|_| "State::save_to")?;
        Ok(())
    }

    /// Apply `func` to the stored value and save it back, returning the updated
    /// value, or `None` if nothing was stored.
    pub fn modify<F>(&self, func: F) -> Result<Option<T>>
    where
        F: for<'a> FnMut(&'a mut T),
    {
        self.modify_at(self.key, func)
    }

    /// Modify the value at an explicit `key`, returning the updated value, or
    /// `None` if nothing was stored.
    pub fn modify_at<F>(&self, key: &str, mut func: F) -> Result<Option<T>>
    where
        F: for<'a> FnMut(&'a mut T),
    {
        if let Some(mut state) = self.load_from(key)? {
            {
                func(&mut state);
            }
            self.save_to(key, &state)?;
            Ok(Some(state))
        } else {
            Ok(None)
        }
    }

    /// Load the value into a guard that saves it back when dropped, or `None`
    /// if nothing is stored.
    pub fn guard(&self) -> Result<Option<Guard<'_, 'k, T, C>>> {
        self.guard_at(self.key)
    }

    /// Like [`State::guard`], but for an explicit `key`.
    pub fn guard_at(&self, key: &str) -> Result<Option<Guard<'_, 'k, T, C>>> {
        Ok(self.load_from(key)?.map(|value| Guard {
            state: self,
            key: String::from(key),
            value,
            save_on_drop: true,
        }))
    }
}

impl<'k, T, C> State<'k, T, C>
where
    T: serde::Serialize + serde::de::DeserializeOwned + Default,
    C: Codec,
{
    /// Return the stored value, inserting `T::default()` when nothing is stored
    /// yet.
    pub fn upsert(&self) -> Result<T> {
        self.upsert_at(self.key)
    }

    /// Like [`State::upsert`], but for an explicit `key`.
    pub fn upsert_at(&self, key: &str) -> Result<T> {
        let state = self.load_from(key)?.unwrap_or_default();
        self.save_to(key, &state)?;
        Ok(state)
    }

    /// Like [`State::upsert`], but applies `func` to the value before saving
    /// it, so it always returns the updated value.
    pub fn upsert_with<F>(&self, func: F) -> Result<T>
    where
        F: for<'a> FnMut(&'a mut T),
    {
        self.upsert_with_at(self.key, func)
    }

    /// Like [`State::upsert_with`], but for an explicit `key`.
    pub fn upsert_with_at<F>(&self, key: &str, mut func: F) -> Result<T>
    where
        F: for<'a> FnMut(&'a mut T),
    {
        let mut state = self.load_from(key)?.unwrap_or_default();
        {
            func(&mut state);
        }
        self.save_to(key, &state)?;
        Ok(state)
    }

    /// Like [`State::guard`], but starts from `T::default()` when nothing is
    /// stored yet, so it always yields a guard.
    pub fn guard_or_default(&self) -> Result<Guard<'_, 'k, T, C>> {
        self.guard_or_default_at(self.key)
    }

    /// Like [`State::guard_or_default`], but for an explicit `key`.
    pub fn guard_or_default_at(&self, key: &str) -> Result<Guard<'_, 'k, T, C>> {
        let value = self.load_from(key)?.unwrap_or_default();
        Ok(Guard {
            state: self,
            key: String::from(key),
            value,
            save_on_drop: true,
        })
    }
}

/// A value borrowed from a [`State`] that is written back when the guard is
/// dropped.
///
/// Access the value through `Deref`/`DerefMut`. The write on drop is
/// best-effort and any error is ignored; call [`Guard::save`] instead when
/// you need to observe a save failure.
pub struct Guard<'s, 'k, T, C = Postcard>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
    C: Codec,
{
    state: &'s State<'k, T, C>,
    key: String,
    value: T,
    save_on_drop: bool,
}

impl<'s, 'k, T, C> Guard<'s, 'k, T, C>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
    C: Codec,
{
    /// Save the value now, returning any error, instead of on drop.
    pub fn save(mut self) -> Result {
        self.save_on_drop = false;
        self.state.save_to(&self.key, &self.value)
    }
}

impl<'s, 'k, T, C> Deref for Guard<'s, 'k, T, C>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
    C: Codec,
{
    type Target = T;

    fn deref(&self) -> &T {
        &self.value
    }
}

impl<'s, 'k, T, C> DerefMut for Guard<'s, 'k, T, C>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
    C: Codec,
{
    fn deref_mut(&mut self) -> &mut T {
        &mut self.value
    }
}

impl<'s, 'k, T, C> Drop for Guard<'s, 'k, T, C>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
    C: Codec,
{
    fn drop(&mut self) {
        if self.save_on_drop {
            let _ = self.state.save_to(&self.key, &self.value);
        }
    }
}
