//! Host functions exposing facts about the runtime hosting this cell: its
//! effective tag set and its runtime id.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::ffi::c_int;

use myrmic_common::types::error::{ENOMEM, SUCCESS};

use crate::{ApiError, ApiResult};

mod c_functions {
    use core::ffi::c_int;

    #[link(wasm_import_module = "runtime")]
    unsafe extern "C" {
        /// Requests the host to report how many bytes the serialized runtime tag
        /// list currently occupies, so the caller can size a buffer for
        /// [`runtime_tags_host`].
        ///
        /// # Returns
        /// - the number of bytes on success (non-negative number)
        /// - [`crate::GENERIC_ERROR`] on error
        pub(super) fn runtime_tags_len_host() -> i32;

        /// Requests the host to write the runtime's effective tag list into the
        /// given buffer, serialized as a `Vec<String>`.
        ///
        /// # Arguments
        /// - buffer: pointer to the module memory where the host shall write the serialized tags
        /// - length: maximal length of the buffer
        ///
        /// # Returns
        /// - [`crate::SUCCESS`] on success
        /// - [`myrmic_common::types::error::ENOMEM`] when the buffer is too small
        /// - [`crate::GENERIC_ERROR`] on error
        pub(super) fn runtime_tags_host(buffer: *mut u8, length: c_int) -> i32;

        /// Requests the host to write the runtime's id (its Zenoh id) into the
        /// given buffer, serialized as a `String`.
        ///
        /// # Arguments
        /// - buffer: pointer to the module memory where the host shall write the serialized id
        /// - length: maximal length of the buffer
        ///
        /// # Returns
        /// - [`crate::SUCCESS`] on success
        /// - [`myrmic_common::types::error::ENOMEM`] when the buffer is too small
        /// - [`crate::GENERIC_ERROR`] on error
        pub(super) fn runtime_id_host(buffer: *mut u8, length: c_int) -> i32;
    }
}

/// The number of times [`runtime_tags`] re-queries the length and retries the
/// fill when the tag set grows between the two host calls (a concurrent retag).
/// A couple of attempts absorbs any realistic churn; the bound guards against a
/// pathological retag storm.
const RUNTIME_TAGS_MAX_ATTEMPTS: usize = 3;

/// A runtime id is a Zenoh id (at most 16 bytes, so at most 32 hex characters);
/// this buffer holds its serialized form with headroom.
const RUNTIME_ID_BUFFER_LEN: usize = 64;

/// Returns the effective tag set of the runtime hosting this cell.
///
/// The tags are the same set self-organization uses to place cells: the
/// runtime's configured tags, any operator retags, and the intrinsic hardware
/// and platform tags (e.g. `embedded`, the board/soc name). Reading them lets a
/// cell adapt to where it landed, for instance to pick a GPIO pin by board or a
/// timer period by whether it runs on a battery-powered device.
#[allow(clippy::cast_sign_loss)]
pub fn runtime_tags() -> ApiResult<Vec<String>> {
    for _ in 0..RUNTIME_TAGS_MAX_ATTEMPTS {
        // SAFETY: the imported function takes no buffer and only reports a length.
        let len = unsafe { c_functions::runtime_tags_len_host() };
        if len < 0 {
            return Err(len.into());
        }

        let mut buf = vec![0u8; len as usize];
        // SAFETY: calling the imported function with pointer and length of guest
        // linear memory owned by this call for this specific purpose.
        let status =
            unsafe { c_functions::runtime_tags_host(buf.as_mut_ptr(), buf.len() as c_int) };
        match status {
            SUCCESS => {
                return postcard::from_bytes(&buf)
                    .map_err(|_e| ApiError::Serde("deserializing runtime tags"));
            }
            // The tag set grew between the length query and the fill; re-query.
            ENOMEM => continue,
            code => return Err(code.into()),
        }
    }

    Err(ApiError::BufferTooSmall)
}

/// Returns the id of the runtime hosting this cell (its Zenoh id) as a string.
///
/// The id is stable for the lifetime of the runtime process. It is the value a
/// deploy pins to via the `@<id>` system tag.
pub fn runtime_id() -> ApiResult<String> {
    let mut buf = [0u8; RUNTIME_ID_BUFFER_LEN];
    // SAFETY: calling the imported function with pointer and length of guest
    // linear memory owned by this call for this specific purpose.
    let status = unsafe { c_functions::runtime_id_host(buf.as_mut_ptr(), buf.len() as c_int) };
    match status {
        SUCCESS => {
            postcard::from_bytes(&buf).map_err(|_e| ApiError::Serde("deserializing runtime id"))
        }
        code => Err(code.into()),
    }
}
