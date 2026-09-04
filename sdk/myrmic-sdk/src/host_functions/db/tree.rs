//! A typed key-value store with hierarchical, prefix-scannable keys.

use core::marker::PhantomData;

use crate::db::{Scope, key_delete, key_get, key_prefix, key_put};
use crate::{Codec, Postcard, Result, String, Vec};

/// A typed key-value store rooted at a static `prefix`.
/// Keys passed to the methods are *relative* to that prefix; the full stored key is
/// `"{prefix}/{key}"`.
///
/// Unlike [`crate::db::table::Table`] (which can only enumerate a whole table), KV supports cheap **prefix scans**,
/// so hierarchical keys like `"{country}/{city}"` can be enumerated one sub-tree at a time via [`Kv::for_each`] / [`Kv::iter`].
///
/// Keys are UTF-8 strings by design — that is what makes the `/`-hierarchy
/// unambiguous and the keys legible in tooling. Binary-keyed rows
/// ([`Sri`](crate::Sri)s, hashes) belong in a
/// [`Table`](crate::db::table::Table) instead, whose entity ids are raw bytes.
pub struct Kv<V, C = Postcard> {
    scope: Scope,
    prefix: &'static str,
    _marker: PhantomData<(V, C)>,
}

impl<V, C> Kv<V, C> {
    /// A store rooted at `prefix` in the default private [`Scope`].
    pub const fn new(prefix: &'static str) -> Self {
        Self::new_in(prefix, Scope::private())
    }

    /// A store rooted at `prefix` in an explicit `scope`.
    pub const fn new_in(prefix: &'static str, scope: Scope) -> Self {
        Self {
            scope,
            prefix,
            _marker: PhantomData,
        }
    }

    fn scope(&self) -> Scope {
        self.scope.clone()
    }
}

impl<V, C> Kv<V, C>
where
    V: serde::Serialize + serde::de::DeserializeOwned,
    C: Codec,
{
    /// `"{prefix}/{key}"` — the full key as stored by the runtime.
    fn full_key(&self, key: &str) -> String {
        let mut k = String::with_capacity(self.prefix.len() + 1 + key.len());
        k.push_str(self.prefix);
        k.push('/');
        k.push_str(key);
        k
    }

    /// Fetches the value under `key`, or `None` if the key is absent.
    pub fn get(&self, key: &str) -> Result<Option<V>> {
        let mut req = [0u8; 256];
        let mut resp = alloc::vec![0u8; 8192];
        key_get(self.scope(), self.full_key(key), &mut req, &mut resp)?
            .map(|b| C::decode(&b))
            .transpose()
    }

    /// Stores `val` under `key`, replacing any existing value.
    pub fn put(&self, key: &str, val: &V) -> Result<()> {
        let bytes = C::encode(val)?;
        let mut buf = alloc::vec![0u8; bytes.len() + 1024];
        key_put(self.scope(), self.full_key(key), bytes, &mut buf).map_err(|_| "Kv::put")?;
        Ok(())
    }

    /// Deletes the value under `key`, if any.
    pub fn delete(&self, key: &str) -> Result<()> {
        let mut buf = [0u8; 256];
        key_delete(self.scope(), self.full_key(key), &mut buf).map_err(|_| "Kv::delete")?;
        Ok(())
    }

    /// Full keys whose relative part starts with `sub` (i.e. stored keys under
    /// `"{prefix}/{sub}"`). The list is small — values are fetched separately.
    pub fn keys(&self, sub: &str) -> Result<Vec<String>> {
        let mut req = alloc::vec![0u8; 1024];
        let mut resp = alloc::vec![0u8; 16384];
        key_prefix(self.scope(), self.full_key(sub), &mut req, &mut resp).map_err(|_| "Kv::keys")
    }

    /// Visit every value under the `sub` prefix, one record at a time
    /// (buffer-safe). Entries that fail to load or decode are skipped.
    pub fn for_each<F: FnMut(V)>(&self, sub: &str, mut f: F) -> Result<()> {
        for key in self.keys(sub)? {
            let mut req = [0u8; 256];
            let mut resp = alloc::vec![0u8; 8192];
            if let Ok(Some(bytes)) = key_get(self.scope(), key, &mut req, &mut resp)
                && let Ok(v) = C::decode::<V>(&bytes)
            {
                f(v);
            }
        }
        Ok(())
    }

    /// Lazily stream the values under the `sub` prefix, surfacing load/decode
    /// errors (unlike [`Self::for_each`], which skips them).
    pub fn iter(&self, sub: &str) -> Result<Iter<V, C>> {
        Ok(Iter {
            scope: self.scope(),
            keys: self.keys(sub)?,
            idx: 0,
            _marker: PhantomData,
        })
    }

    /// Collect every value under the `sub` prefix.
    pub fn list(&self, sub: &str) -> Result<Vec<V>> {
        self.iter(sub)?.collect()
    }
}

/// Lazy iterator over the values under a prefix. Keys are resolved up front
/// (they are small); each value is fetched on demand.
pub struct Iter<V, C = Postcard> {
    scope: Scope,
    keys: Vec<String>,
    idx: usize,
    _marker: PhantomData<(V, C)>,
}

impl<V, C> Iterator for Iter<V, C>
where
    V: serde::de::DeserializeOwned,
    C: Codec,
{
    type Item = Result<V>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let key = self.keys.get(self.idx)?.clone();
            self.idx += 1;
            let mut req = [0u8; 256];
            let mut resp = alloc::vec![0u8; 8192];
            match key_get(self.scope.clone(), key, &mut req, &mut resp) {
                Ok(Some(bytes)) => return Some(C::decode::<V>(&bytes)),
                Ok(None) => continue, // key vanished between scan and fetch
                Err(_) => return Some(Err("Kv::iter")),
            }
        }
    }
}
