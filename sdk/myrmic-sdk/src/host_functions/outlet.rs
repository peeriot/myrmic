//! Safe wrappers for the "outlet" host module — Signal Layer outlet discovery
//! and writing.
//!
//! An outlet is the write-side mirror of a [`Tap`](super::tap::Tap): a named,
//! typed slot a cell drives and a backing driver consumes. The last write wins;
//! there is no arbitration (single-writer ownership is resolved statically at
//! manifest/codegen time).

use core::ffi::c_int;

use myrmic_common::types::error::EINVAL;

use super::tap::TapKind;
use signal_layer_types::WireType;

use crate::error::{ApiError, ApiResult};

mod c_functions {
    use core::ffi::c_int;

    #[link(wasm_import_module = "outlet")]
    unsafe extern "C" {
        /// Resolve an outlet name to an integer handle.
        ///
        /// # Arguments
        /// - `name_ptr`: pointer to the UTF-8 outlet name
        /// - `name_len`: byte length of the name
        ///
        /// # Returns
        /// - handle (`>= 0`) on success
        /// - `-1` if the name is not registered
        /// - `GENERIC_ERROR` on other errors (null pointer, invalid UTF-8, registry not initialised)
        pub(super) fn outlet_resolve(name_ptr: *const u8, name_len: c_int) -> c_int;

        /// Write a postcard-encoded command into the outlet as its latest value.
        ///
        /// # Arguments
        /// - `handle`: outlet handle returned by `outlet_resolve`
        /// - `buf`: pointer to the encoded command payload
        /// - `buf_len`: byte length of the payload
        ///
        /// # Returns
        /// - `0` on success
        /// - `EINVAL` if the payload does not decode into the outlet's declared type
        /// - `ESTALE` if the handle is stale/never issued or the backing service is gone
        /// - negative on other errors (wrong slot kind, registry not initialised)
        pub(super) fn outlet_write_retained(handle: c_int, buf: *const u8, buf_len: c_int)
        -> c_int;

        /// Returns the total number of registered outlets.
        ///
        /// # Returns
        /// - outlet count (`>= 0`) on success
        /// - `GENERIC_ERROR` if the registry is not initialised
        pub(super) fn outlet_list_len() -> c_int;

        /// Write the name of the outlet at `index` into `name_ptr[0..name_len]`.
        ///
        /// # Arguments
        /// - `index`: zero-based outlet index
        /// - `name_ptr`: pointer to the output buffer; if null the name is not written
        /// - `name_len`: capacity of the output buffer in bytes
        /// - `out_kind_buf`: if non-null, receives the slot kind (0 = retained) as a
        ///   little-endian `i32`; must be exactly 4 bytes
        /// - `out_kind_len`: length of `out_kind_buf` in bytes (WAMR validates the full
        ///   `out_kind_buf[0..out_kind_len]` range; the host requires `out_kind_len == 4`)
        ///
        /// # Returns
        /// - bytes written (or that would be written if `name_ptr` is null) on success
        /// - `-1` if `index` is out of bounds
        pub(super) fn outlet_list_entry(
            index: c_int,
            name_ptr: *mut u8,
            name_len: c_int,
            out_kind_buf: *mut u8,
            out_kind_len: c_int,
        ) -> c_int;

        /// Write the declared command type of the outlet at `handle` into
        /// `*out_id` (a `u8` buffer of `out_id_len` bytes; the host requires
        /// `out_id_len == 4` and writes the id little-endian). Returns `0` on
        /// success or a negative code on error (unknown handle, bad length,
        /// registry not initialised).
        pub(super) fn outlet_type_id(handle: c_int, out_id: *mut u8, out_id_len: c_int) -> c_int;
    }
}

/// A resolved outlet handle — wraps the integer handle returned by the host.
///
/// Obtained via [`Outlet::resolve`]. The handle is opaque; use the methods on
/// this type to write to the outlet rather than passing raw integers across the
/// FFI boundary.
#[derive(Debug)]
pub struct Outlet {
    handle: u32,
    /// The outlet's declared command type, fetched once at resolve time;
    /// typed writes are checked against it (swarm#1315).
    type_id: u32,
}

impl Outlet {
    /// Resolve an outlet name to an `Outlet`.
    ///
    /// Returns `Ok(Some(Outlet))` on success, `Ok(None)` if the name is not registered,
    /// or `Err` on other errors (e.g. registry not initialised, invalid UTF-8).
    pub fn resolve(name: &str) -> ApiResult<Option<Outlet>> {
        // SAFETY: `name` is a valid UTF-8 `&str`; pointer and length are consistent
        // and the string outlives the host call.
        let n = unsafe { c_functions::outlet_resolve(name.as_ptr(), name.len() as c_int) };
        let handle = match n {
            n if n >= 0 => n as u32,
            -1 => return Ok(None),
            n => return Err(ApiError::from(n)),
        };

        let mut id_buf = [0u8; core::mem::size_of::<u32>()];
        // SAFETY: `id_buf` is a valid `&mut [u8]` paired with its own length and
        // outlives the host call; the host writes the id as little-endian bytes.
        let status = unsafe {
            c_functions::outlet_type_id(handle as c_int, id_buf.as_mut_ptr(), id_buf.len() as c_int)
        };
        if status != 0 {
            return Err(ApiError::from(status));
        }
        Ok(Some(Self {
            handle,
            type_id: u32::from_le_bytes(id_buf),
        }))
    }

    /// The outlet's declared command type ([`WireType::TYPE_ID`]).
    #[must_use]
    pub fn wire_type_id(&self) -> u32 {
        self.type_id
    }

    /// Write a raw postcard-encoded command into this outlet.
    ///
    /// Returns `Ok(())` on success, or `Err(ApiError::Serde)` if the host rejects
    /// the payload because it does not decode into the outlet's declared type.
    /// [`ApiError::Unavailable`] means the handle can no longer serve writes (the
    /// pipeline went away or the handle predates a reconnect) — re-resolve the
    /// outlet.
    ///
    /// Most callers should use [`write_typed`](Self::write_typed) instead — this
    /// raw path is the documented escape hatch and performs **no** cell-side
    /// wire-type check; the host still refuses payloads that do not decode
    /// exactly into the declared type.
    pub fn write(&self, bytes: &[u8]) -> ApiResult<()> {
        // SAFETY: `bytes` is a valid `&[u8]`; pointer and length are consistent
        // and the slice outlives the host call.
        let status = unsafe {
            c_functions::outlet_write_retained(
                self.handle as c_int,
                bytes.as_ptr(),
                bytes.len() as c_int,
            )
        };
        match status {
            0 => Ok(()),
            n if n == EINVAL => Err(ApiError::Serde("outlet rejected command payload")),
            n => Err(ApiError::from(n)),
        }
    }

    /// Encode `value` via postcard and write it into this outlet as the latest command.
    ///
    /// The internal scratch buffer is 64 bytes; a command that serializes larger
    /// than that yields `ApiError::BufferTooSmall`. Returns `Err(ApiError::Serde)`
    /// if the host rejects the encoded payload (wrong declared type).
    ///
    /// `T` is checked against the outlet's declared command type before
    /// anything crosses the boundary (swarm#1315): a mismatch returns
    /// [`ApiError::TypeMismatch`] and the hardware never sees the write.
    pub fn write_typed<T: serde::Serialize + WireType>(&self, value: &T) -> ApiResult<()> {
        if T::TYPE_ID != self.type_id {
            return Err(ApiError::TypeMismatch {
                expected: T::TYPE_ID,
                actual: self.type_id,
            });
        }
        let mut buf = [0u8; 64];
        let used = postcard::to_slice(value, &mut buf).map_err(|_| ApiError::BufferTooSmall)?;
        self.write(used)
    }
}

/// Returns the number of outlets registered in the host's outlet registry.
///
/// Returns `Err` if the registry is not initialised.
pub fn list_len() -> ApiResult<u32> {
    // SAFETY: no pointer arguments; call is unconditionally safe.
    let n = unsafe { c_functions::outlet_list_len() };
    if n >= 0 {
        Ok(n as u32)
    } else {
        Err(ApiError::from(n))
    }
}

/// Returns the name and kind of the outlet at `index`, writing the name into `name_buf`.
///
/// Returns `Ok(Some((bytes_written, kind)))` on success, `Ok(None)` if `index` is out
/// of bounds, or `Err` on other errors (e.g. registry not initialised).
pub fn list_entry(index: u32, name_buf: &mut [u8]) -> ApiResult<Option<(usize, TapKind)>> {
    let mut kind_buf = [0u8; size_of::<c_int>()];
    // SAFETY: `name_buf` and `kind_buf` are valid, non-overlapping `&mut [u8]`s; each
    // pointer is paired with its own length and both slices outlive the host call. The
    // host writes the kind as little-endian bytes, so `kind_buf` needs no alignment.
    let n = unsafe {
        c_functions::outlet_list_entry(
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
