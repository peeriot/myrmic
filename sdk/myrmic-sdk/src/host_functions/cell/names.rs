//! Looking up the name a cell was registered or spawned under.

use core::ffi::c_int;

use myrmic_common::types::error::{ENOMEM, SUCCESS};

use crate::{ApiError, ApiResult, Sri};

mod c_functions {
    use core::ffi::c_int;

    #[link(wasm_import_module = "cell")]
    unsafe extern "C" {
        /// Requests the host to write the name the cell named by the given
        /// SRI was registered or spawned under into the given buffer,
        /// serialized as an `Option<String>`.
        ///
        /// # Arguments
        /// - sri: pointer to the postcard-serialized SRI to look up
        /// - sri_length: its length
        /// - buffer: pointer to the module memory where the host shall write the serialized name
        /// - length: maximal length of the buffer
        ///
        /// # Returns
        /// - [`crate::SUCCESS`] on success
        /// - [`myrmic_common::types::error::ENOMEM`] when the buffer is too small
        /// - [`crate::GENERIC_ERROR`] when the name can't be looked up
        pub(super) fn cell_name_host(
            sri: *const u8,
            sri_length: c_int,
            buffer: *mut u8,
            length: c_int,
        ) -> i32;
    }
}

/// The longest name [`name_of`] returns, in bytes.
pub const CELL_NAME_MAX_LEN: usize = 64;

/// A cell's name, as [`name_of`] returns it.
pub type CellName = heapless::String<CELL_NAME_MAX_LEN>;

/// The serialized `Option<String>` the host writes: an option tag, the length
/// as a varint (two bytes covers 64), then the name.
const NAME_BUFFER_LEN: usize = CELL_NAME_MAX_LEN + 3;

/// The name the cell `sri` was registered or spawned under, which an SRI can't
/// be turned back into on its own.
///
/// That is the SRN a native cell registered with (`Network::register`), or the
/// local name a spawned child was given. A root deployed by SRN (`myrmic
/// deploy --name`) has no name recorded, and neither does an SRI no cell has
/// ever had; both are `None`.
///
/// **This is not cheap.** Every call reads the instance registry on another
/// node: a full db round trip, made while this handler waits. Call it rarely —
/// once per cell you need the name of, for instance when it first shows up —
/// and keep the result, rather than looking it up on every message.
///
/// Linux runtime only.
///
/// # Errors
///
/// [`ApiError::BufferTooSmall`] if the name is longer than
/// [`CELL_NAME_MAX_LEN`] bytes, and the host's error if the registry can't be
/// read.
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
pub fn name_of(sri: Sri) -> ApiResult<Option<CellName>> {
    let mut sri_buf = [0u8; 32];
    let sri_bytes = postcard::to_slice(&sri, &mut sri_buf)
        .map_err(|_e| ApiError::Serde("serializing the SRI to look up"))?;

    let mut buf = [0u8; NAME_BUFFER_LEN];
    // SAFETY: calling the imported function with pointers and lengths of guest
    // linear memory owned by this call for this specific purpose.
    let status = unsafe {
        c_functions::cell_name_host(
            sri_bytes.as_ptr(),
            sri_bytes.len() as c_int,
            buf.as_mut_ptr(),
            buf.len() as c_int,
        )
    };
    match status {
        SUCCESS => {
            postcard::from_bytes(&buf).map_err(|_e| ApiError::Serde("deserializing a cell name"))
        }
        ENOMEM => Err(ApiError::BufferTooSmall),
        code => Err(code.into()),
    }
}
