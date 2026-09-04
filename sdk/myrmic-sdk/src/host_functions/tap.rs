//! Safe wrappers for the "tap" host module — Signal Layer tap discovery and reading.

use core::ffi::c_int;

use myrmic_common::types::error::EINVAL;

use signal_layer_types::WireType;

use crate::error::{ApiError, ApiResult};

mod c_functions {
    use core::ffi::c_int;

    #[link(wasm_import_module = "tap")]
    unsafe extern "C" {
        /// Resolve a tap name to an integer handle.
        ///
        /// # Arguments
        /// - `name_ptr`: pointer to the UTF-8 tap name
        /// - `name_len`: byte length of the name
        ///
        /// # Returns
        /// - handle (`>= 0`) on success
        /// - `-1` if the name is not registered
        /// - [`crate::GENERIC_ERROR`] on other errors (null pointer, invalid UTF-8, registry not initialised)
        pub(super) fn tap_resolve(name_ptr: *const u8, name_len: c_int) -> c_int;

        /// Read the latest retained value for `handle` into `buf`.
        ///
        /// # Arguments
        /// - `handle`: tap handle returned by `tap_resolve`
        /// - `buf`: pointer to the output buffer
        /// - `buf_len`: capacity of the output buffer in bytes
        /// - `ts_out_buf`: if non-null, receives the timestamp (ms since boot) of the
        ///   stored value as a little-endian `u64`; must be exactly 8 bytes
        /// - `ts_out_len`: length of `ts_out_buf` in bytes (WAMR validates the full
        ///   `ts_out_buf[0..ts_out_len]` range; the host requires `ts_out_len == 8`)
        ///
        /// # Returns
        /// - bytes written (`> 0`) on success
        /// - `0` if no value has been stored yet
        /// - [`crate::EINVAL`] if the supplied buffer is too small for the serialized value
        /// - negative on error (wrong slot kind, invalid handle, timestamp buffer too small, …)
        pub(super) fn tap_read_retained(
            handle: c_int,
            buf: *mut u8,
            buf_len: c_int,
            ts_out_buf: *mut u8,
            ts_out_len: c_int,
        ) -> c_int;

        /// Take the next pending event for `handle` into `buf`.
        ///
        /// # Arguments
        /// - `handle`: tap handle returned by `tap_resolve`
        /// - `buf`: pointer to the output buffer
        /// - `buf_len`: capacity of the output buffer in bytes
        ///
        /// # Returns
        /// - bytes written (`> 0`) on success
        /// - `0` if the event queue is empty
        /// - [`crate::EINVAL`] if the supplied buffer is too small for the serialized event
        /// - negative on error (wrong slot kind, invalid handle, …)
        pub(super) fn tap_take_event(handle: c_int, buf: *mut u8, buf_len: c_int) -> c_int;

        /// Returns the total number of registered taps.
        ///
        /// # Returns
        /// - tap count (`>= 0`) on success
        /// - [`crate::GENERIC_ERROR`] if the registry is not initialised
        pub(super) fn tap_list_len() -> c_int;

        /// Write the name of the tap at `index` into `name_ptr[0..name_len]`.
        ///
        /// # Arguments
        /// - `index`: zero-based tap index
        /// - `name_ptr`: pointer to the output buffer; if null the name is not written
        /// - `name_len`: capacity of the output buffer in bytes
        /// - `out_kind_buf`: if non-null, receives the slot kind (0 = retained, 1 = event,
        ///   2 = batch) as a little-endian `i32`; must be exactly 4 bytes
        /// - `out_kind_len`: length of `out_kind_buf` in bytes (WAMR validates the full
        ///   `out_kind_buf[0..out_kind_len]` range; the host requires `out_kind_len == 4`)
        ///
        /// # Returns
        /// - bytes written (or that would be written if `name_ptr` is null) on success
        /// - `-1` if `index` is out of bounds
        pub(super) fn tap_list_entry(
            index: c_int,
            name_ptr: *mut u8,
            name_len: c_int,
            out_kind_buf: *mut u8,
            out_kind_len: c_int,
        ) -> c_int;

        /// Write the declared wire type of the slot at `handle` into
        /// `*out_id` (a `u8` buffer of `out_id_len` bytes; the host requires
        /// `out_id_len == 4` and writes the id little-endian). Returns `0` on
        /// success or a negative code on error (unknown handle, bad length,
        /// registry not initialised).
        pub(super) fn tap_type_id(handle: c_int, out_id: *mut u8, out_id_len: c_int) -> c_int;
    }
}

/// Kind of a tap slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TapKind {
    /// A last-value slot: the tap retains its most recent value, read via
    /// [`Tap::read_retained`].
    Retained,
    /// An event slot: each value is a discrete event, consumed via
    /// [`Tap::take_event`].
    Event,
    /// A batch slot: values accumulate and are read out in batches.
    Batch,
    /// A slot kind this SDK version does not know about.
    Unknown(i32),
}

impl From<c_int> for TapKind {
    fn from(v: c_int) -> Self {
        match v {
            0 => TapKind::Retained,
            1 => TapKind::Event,
            2 => TapKind::Batch,
            n => TapKind::Unknown(n),
        }
    }
}

/// A resolved tap handle — wraps the integer handle returned by the host.
///
/// Obtained via [`Tap::resolve`]. The handle is opaque; use the methods on this
/// type to read from the tap rather than passing raw integers across the FFI boundary.
#[derive(Debug)]
pub struct Tap {
    handle: u32,
    /// The slot's declared wire type, fetched once at resolve time; typed
    /// reads are checked against it (swarm#1315).
    type_id: u32,
}

impl Tap {
    /// Resolve a tap name to a `Tap`.
    ///
    /// Returns `Ok(Some(Tap))` on success, `Ok(None)` if the name is not registered,
    /// or `Err` on other errors (e.g. registry not initialised, invalid UTF-8).
    pub fn resolve(name: &str) -> ApiResult<Option<Tap>> {
        // SAFETY: `name` is a valid UTF-8 `&str`; pointer and length are consistent
        // and the string outlives the host call.
        let n = unsafe { c_functions::tap_resolve(name.as_ptr(), name.len() as c_int) };
        let handle = match n {
            n if n >= 0 => n as u32,
            -1 => return Ok(None),
            n => return Err(ApiError::from(n)),
        };

        let mut id_buf = [0u8; core::mem::size_of::<u32>()];
        // SAFETY: `id_buf` is a valid `&mut [u8]` paired with its own length and
        // outlives the host call; the host writes the id as little-endian bytes.
        let status = unsafe {
            c_functions::tap_type_id(handle as c_int, id_buf.as_mut_ptr(), id_buf.len() as c_int)
        };
        if status != 0 {
            return Err(ApiError::from(status));
        }
        Ok(Some(Self {
            handle,
            type_id: u32::from_le_bytes(id_buf),
        }))
    }

    /// The slot's declared wire type ([`WireType::TYPE_ID`]).
    #[must_use]
    pub fn wire_type_id(&self) -> u32 {
        self.type_id
    }

    /// Read the latest retained value for this `Tap` into `buf`.
    ///
    /// Returns `Ok(Some((bytes_written, timestamp_ms)))` on success, `Ok(None)` if no value
    /// has been stored yet, or `Err` if the internal handle is invalid or the slot is the wrong kind.
    /// [`ApiError::Unavailable`] means the handle can no longer serve reads (the
    /// pipeline went away or the handle predates a reconnect) — re-resolve the tap.
    pub fn read_retained(&self, buf: &mut [u8]) -> ApiResult<Option<(usize, u64)>> {
        let mut ts_buf = [0u8; size_of::<u64>()];
        // SAFETY: `buf` and `ts_buf` are valid, non-overlapping `&mut [u8]`s; each pointer
        // is paired with its own length and both slices outlive the host call. The host
        // writes the timestamp as little-endian bytes, so `ts_buf` needs no alignment.
        let n = unsafe {
            c_functions::tap_read_retained(
                self.handle as c_int,
                buf.as_mut_ptr(),
                buf.len() as c_int,
                ts_buf.as_mut_ptr(),
                ts_buf.len() as c_int,
            )
        };
        match n {
            0 => Ok(None),
            n if n > 0 => Ok(Some((n as usize, u64::from_le_bytes(ts_buf)))),
            n if n == EINVAL => Err(ApiError::BufferTooSmall),
            n => Err(ApiError::from(n)),
        }
    }

    /// Take the next pending event for this `Tap` into `buf`.
    ///
    /// Returns `Ok(Some(bytes_written))` on success, `Ok(None)` if the queue is empty,
    /// or `Err` if the internal handle is invalid or the slot is the wrong kind.
    /// [`ApiError::Unavailable`] means the handle can no longer serve reads (the
    /// pipeline went away or the handle predates a reconnect) — re-resolve the tap.
    pub fn take_event(&self, buf: &mut [u8]) -> ApiResult<Option<usize>> {
        // SAFETY: `buf` is a valid `&mut [u8]`; pointer and length are consistent
        // and the slice outlives the host call.
        let n = unsafe {
            c_functions::tap_take_event(self.handle as c_int, buf.as_mut_ptr(), buf.len() as c_int)
        };
        match n {
            0 => Ok(None),
            n if n > 0 => Ok(Some(n as usize)),
            n if n == EINVAL => Err(ApiError::BufferTooSmall),
            n => Err(ApiError::from(n)),
        }
    }

    /// Decode the latest retained value for this tap into `T` via postcard.
    ///
    /// Returns `Ok(Some((timestamp_ms, value)))` when a value is present and
    /// decodes successfully, `Ok(None)` when no value has been stored yet, or
    /// `Err` on a host error or a postcard decode failure.
    ///
    /// The internal scratch buffer is 64 bytes. If the serialized payload
    /// exceeds that, the host returns `ApiError::BufferTooSmall`; use
    /// [`read_retained`](Self::read_retained) with a larger buffer in that case.
    ///
    /// `T` is checked against the slot's declared wire type before decoding
    /// (swarm#1315): a mismatch returns [`ApiError::TypeMismatch`] instead of
    /// a plausible wrong value. The decode is strict — trailing bytes are a
    /// decode error, never silently discarded.
    pub fn read_typed<T: serde::de::DeserializeOwned + WireType>(
        &self,
    ) -> ApiResult<Option<(u64, T)>> {
        self.check_type::<T>()?;
        let mut buf = [0u8; 64];
        match self.read_retained(&mut buf)? {
            None => Ok(None),
            Some((n, ts)) => {
                let value = decode_strict::<T>(&buf[..n])?;
                Ok(Some((ts, value)))
            }
        }
    }

    fn check_type<T: WireType>(&self) -> ApiResult<()> {
        if T::TYPE_ID != self.type_id {
            return Err(ApiError::TypeMismatch {
                expected: T::TYPE_ID,
                actual: self.type_id,
            });
        }
        Ok(())
    }

    /// Take and decode the next pending event for this tap into `T` via postcard.
    ///
    /// Returns `Ok(Some(value))` when an event is available and decodes
    /// successfully, `Ok(None)` when the queue is empty, or `Err` on a host
    /// error or a postcard decode failure.
    ///
    /// **The event is consumed before decoding.** A decode error (wrong type
    /// `T`, truncated buffer) is not retryable — the event is permanently
    /// lost. Use [`Self::take_event`] if you need the raw bytes to recover from a
    /// decode failure.
    ///
    /// The internal scratch buffer is 64 bytes. If the serialized payload
    /// exceeds that, the host returns `ApiError::BufferTooSmall`; use
    /// [`take_event`](Self::take_event) with a larger buffer in that case.
    ///
    /// `T` is checked against the slot's declared wire type **before** the
    /// event is consumed (swarm#1315), so a [`ApiError::TypeMismatch`] does
    /// not lose the event.
    pub fn take_event_typed<T: serde::de::DeserializeOwned + WireType>(
        &self,
    ) -> ApiResult<Option<T>> {
        self.check_type::<T>()?;
        let mut buf = [0u8; 64];
        match self.take_event(&mut buf)? {
            None => Ok(None),
            Some(n) => Ok(Some(decode_strict::<T>(&buf[..n])?)),
        }
    }
}

/// Postcard decode that refuses trailing bytes: a payload with a remainder
/// was produced for a different type whose prefix happened to parse.
fn decode_strict<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> ApiResult<T> {
    let (value, rest) = postcard::take_from_bytes::<T>(bytes)
        .map_err(|_| ApiError::Serde("postcard decode failed"))?;
    if !rest.is_empty() {
        return Err(ApiError::Serde("trailing bytes after decoded value"));
    }
    Ok(value)
}

/// Returns the number of taps registered in the host's tap registry.
///
/// Returns `Err` if the registry is not initialised.
pub fn list_len() -> ApiResult<u32> {
    // SAFETY: no pointer arguments; call is unconditionally safe.
    let n = unsafe { c_functions::tap_list_len() };
    if n >= 0 {
        Ok(n as u32)
    } else {
        Err(ApiError::from(n))
    }
}

/// Returns the name and kind of the tap at `index`, writing the name into `name_buf`.
///
/// Returns `Ok(Some((bytes_written, kind)))` on success, `Ok(None)` if `index` is out
/// of bounds, or `Err` on other errors (e.g. registry not initialised).
pub fn list_entry(index: u32, name_buf: &mut [u8]) -> ApiResult<Option<(usize, TapKind)>> {
    let mut kind_buf = [0u8; size_of::<c_int>()];
    // SAFETY: `name_buf` and `kind_buf` are valid, non-overlapping `&mut [u8]`s; each
    // pointer is paired with its own length and both slices outlive the host call. The
    // host writes the kind as little-endian bytes, so `kind_buf` needs no alignment.
    let n = unsafe {
        c_functions::tap_list_entry(
            index as c_int,
            name_buf.as_mut_ptr(),
            name_buf.len() as c_int,
            kind_buf.as_mut_ptr(),
            kind_buf.len() as c_int,
        )
    };
    match n {
        n if n >= 0 => Ok(Some((
            n as usize,
            TapKind::from(c_int::from_le_bytes(kind_buf)),
        ))),
        -1 => Ok(None),
        n => Err(ApiError::from(n)),
    }
}
