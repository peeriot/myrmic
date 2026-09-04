use core::cell::{Ref, RefCell, RefMut};

/// Transient, cell-local storage for state that must not be persisted, such as host resource
/// handles (sockets, subscriptions, scan sessions, ...) held across handler invocations.
///
/// Unlike `State`, `InMemory` never touches the data layer: the wrapped value only lives for as
/// long as the cell instance is loaded and is lost on restart. It is typically stored in a
/// `static` and accessed through [`InMemory::with`].
///
/// Wrap the value in an [`Option`] and declare it with [`InMemory::empty`] when it only becomes
/// available at runtime, such as a handle returned by a host call.
pub struct InMemory<C> {
    value: RefCell<C>,
}

// SAFETY: The `InMemory` is only used in a single-threaded context, so it is safe to implement
// `Sync` for it. The `RefCell` ensures that the inner value cannot be accessed re-entrantly.
unsafe impl<C> Sync for InMemory<C> {}

impl<C> InMemory<C> {
    /// Wraps `value` in a new `InMemory`.
    pub const fn new(value: C) -> Self {
        Self {
            value: RefCell::new(value),
        }
    }

    /// Runs `f` with mutable access to the wrapped value.
    ///
    /// This covers the common case of reading or updating the value in a single expression. If
    /// you need to interleave the access with control flow in the caller, such as an early
    /// `return`, a `return` inside `f` only exits `f` itself; use [`InMemory::try_borrow_mut`]
    /// or [`InMemory::try_borrow`] instead, since the guard they return lives in the caller's
    /// own scope.
    ///
    /// # Errors
    ///
    /// Returns an error if `with` is called re-entrantly, i.e. from within another call to
    /// `with`/`upsert_with`/`try_borrow`/`try_borrow_mut` on the same `InMemory` (for example,
    /// from a callback invoked by `f`).
    pub fn with<R>(&self, f: impl FnOnce(&mut C) -> R) -> crate::Result<R> {
        let mut value = self.try_borrow_mut()?;

        Ok(f(&mut value))
    }

    /// Immutably borrows the wrapped value.
    ///
    /// Prefer this over [`InMemory::with`] when the access needs to be interleaved with control
    /// flow in the caller, since the returned guard lives in the caller's own scope rather than
    /// inside a closure.
    ///
    /// # Errors
    ///
    /// Returns an error if the value is currently mutably borrowed, i.e. from a re-entrant call
    /// to `with`/`upsert_with`/`try_borrow_mut` (for example, from within a callback).
    pub fn try_borrow(&self) -> crate::Result<Ref<'_, C>> {
        self.value
            .try_borrow()
            .map_err(|_| "In-memory value was already mutably borrowed. Do not use re-entrantly.")
    }

    /// Mutably borrows the wrapped value.
    ///
    /// Prefer this over [`InMemory::with`] when the access needs to be interleaved with control
    /// flow in the caller, since the returned guard lives in the caller's own scope rather than
    /// inside a closure.
    ///
    /// # Errors
    ///
    /// Returns an error if the value is already borrowed, i.e. from a re-entrant call to
    /// `with`/`upsert_with`/`try_borrow`/`try_borrow_mut` (for example, from within a callback).
    pub fn try_borrow_mut(&self) -> crate::Result<RefMut<'_, C>> {
        self.value
            .try_borrow_mut()
            .map_err(|_| "In-memory value was already borrowed. Do not use re-entrantly.")
    }
}

impl<T> InMemory<Option<T>> {
    /// Declares a `InMemory` that starts out without a value.
    ///
    /// Use this for a value that can only be built at runtime, such as a handle returned by a
    /// host call. The [`Option`] is part of the stored type, so [`InMemory::with`] hands the
    /// closure an `&mut Option<T>` and the caller deals with exactly one level of optionality:
    ///
    /// ```
    /// # use myrmic_sdk::InMemory;
    /// # struct ScanHandle;
    /// # impl ScanHandle { fn stop(self) -> myrmic_sdk::Result<()> { Ok(()) } }
    /// # fn demo(scan: ScanHandle) -> myrmic_sdk::Result<()> {
    /// static SCAN: InMemory<Option<ScanHandle>> = InMemory::empty();
    ///
    /// SCAN.with(|slot| *slot = Some(scan))?;
    ///
    /// if let Some(scan) = SCAN.with(Option::take)? {
    ///     scan.stop()?;
    /// }
    /// # Ok(())
    /// # }
    /// # demo(ScanHandle).unwrap();
    /// ```
    ///
    /// When `T` implements [`Default`], [`InMemory::upsert_with`] hands the closure an `&mut T`
    /// instead, inserting the default value first.
    pub const fn empty() -> Self {
        Self::new(None)
    }
}

impl<T> InMemory<Option<T>>
where
    T: Default,
{
    /// Runs `f` with mutable access to the wrapped value, inserting `T::default()` first if there
    /// is no value yet.
    ///
    /// This mirrors the `upsert` family on `State` for a value that is never persisted. Since the
    /// insert guarantees a value, `f` receives an `&mut T` and its result is not wrapped in an
    /// [`Option`], which makes this the more direct way to build a value up field by field across
    /// several invocations:
    ///
    /// ```
    /// # use myrmic_sdk::InMemory;
    /// # #[derive(Default)]
    /// # struct Session { scan: Option<u32> }
    /// # fn demo(scan: u32) -> myrmic_sdk::Result<()> {
    /// static SESSION: InMemory<Option<Session>> = InMemory::empty();
    ///
    /// SESSION.upsert_with(|session| session.scan = Some(scan))?;
    /// # Ok(())
    /// # }
    /// # demo(7).unwrap();
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if `upsert_with` is called re-entrantly, i.e. from within another call to
    /// `with`/`upsert_with`/`try_borrow`/`try_borrow_mut` on the same `InMemory` (for example,
    /// from a callback invoked by `f`).
    pub fn upsert_with<R>(&self, f: impl FnOnce(&mut T) -> R) -> crate::Result<R> {
        let mut slot = self.try_borrow_mut()?;

        Ok(f(slot.get_or_insert_default()))
    }
}

#[cfg(test)]
mod tests {
    use super::InMemory;

    #[derive(Default)]
    struct Handle {
        id: u32,
        retries: u8,
    }

    #[test]
    fn present_value_needs_no_option() {
        static CTX: InMemory<u32> = InMemory::new(7);

        let doubled: u32 = CTX.with(|value| *value * 2).unwrap();
        assert_eq!(doubled, 14);
    }

    #[test]
    fn present_value_updates_individual_fields() {
        static CTX: InMemory<Handle> = InMemory::new(Handle { id: 1, retries: 0 });

        CTX.with(|handle| handle.retries += 1).unwrap();
        CTX.with(|handle| handle.id = 9).unwrap();

        let fields = CTX.with(|handle| (handle.id, handle.retries)).unwrap();
        assert_eq!(fields, (9, 1));
    }

    #[test]
    fn empty_slot_sets_and_takes() {
        static CTX: InMemory<Option<Handle>> = InMemory::empty();

        assert!(CTX.with(|slot| slot.is_none()).unwrap());
        CTX.with(|slot| *slot = Some(Handle { id: 3, retries: 0 }))
            .unwrap();

        let taken: Option<Handle> = CTX.with(Option::take).unwrap();
        assert_eq!(taken.map(|handle| handle.id), Some(3));
        assert!(CTX.with(|slot| slot.is_none()).unwrap());
    }

    #[test]
    fn empty_slot_upserts_individual_fields() {
        static CTX: InMemory<Option<Handle>> = InMemory::empty();

        // An empty slot is filled field by field, so a value that only becomes complete over
        // several invocations needs no accessor of its own.
        CTX.upsert_with(|handle| handle.id = 7).unwrap();

        // The closure's result comes back unwrapped, because the insert guarantees a value.
        let retries = CTX
            .upsert_with(|handle| {
                handle.retries += 1;

                handle.retries
            })
            .unwrap();
        assert_eq!(retries, 1);

        // A field of a value that is already there is updated in place.
        CTX.with(|slot| {
            if let Some(handle) = slot {
                handle.retries += 1;
            }
        })
        .unwrap();

        let fields = CTX
            .with(|slot| slot.as_ref().map(|handle| (handle.id, handle.retries)))
            .unwrap();
        assert_eq!(fields, Some((7, 2)));
    }

    #[test]
    fn reentrant_access_is_rejected() {
        static CTX: InMemory<u32> = InMemory::new(7);

        assert!(CTX.with(|_| CTX.with(|_| ())).unwrap().is_err());
    }
}
