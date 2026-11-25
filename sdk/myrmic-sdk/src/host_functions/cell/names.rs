//! Looking up the name a cell was registered or spawned under.

use core::ffi::c_int;

use myrmic_common::types::error::{ENOENT, ENOMEM};

use crate::{ApiError, ApiResult, Sri};

mod c_functions {
    use core::ffi::c_int;

    #[link(wasm_import_module = "cell")]
    unsafe extern "C" {
        /// Requests the host to write the name the cell named by the given
        /// SRI was registered or spawned under into the given buffer, as raw
        /// UTF-8.
        ///
        /// # Arguments
        /// - sri: pointer to the postcard-serialized SRI to look up
        /// - sri_length: its length
        /// - buffer: pointer to the module memory where the host shall write the name
        /// - length: maximal length of the buffer
        ///
        /// # Returns
        /// - the name's length in bytes (non-negative) on success
        /// - [`myrmic_common::types::error::ENOENT`] when no name is recorded
        /// - [`myrmic_common::types::error::ENOMEM`] when the name doesn't fit the buffer
        /// - [`crate::GENERIC_ERROR`] when the name can't be looked up
        pub(super) fn cell_name_host(
            sri: *const u8,
            sri_length: c_int,
            buffer: *mut u8,
            length: c_int,
        ) -> i32;
    }
}

/// The name the cell `sri` was registered or spawned under, which an SRI can't
/// be turned back into on its own, in a string of up to `N` bytes.
///
/// That is the SRN a native cell registered with (`Network::register`), or the
/// local name a spawned child was given. A root deployed by SRN (`myrmic
/// deploy --name`) has no name recorded, and neither does an SRI no cell has
/// ever had; both are `None`.
///
/// ```ignore
/// let name = myrmic_sdk::name_of::<32>(md.sender)?;
/// ```
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
/// [`ApiError::BufferTooSmall`] if the name is longer than `N` bytes, and the
/// host's error if the registry can't be read.
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
pub fn name_of<const N: usize>(sri: Sri) -> ApiResult<Option<heapless::String<N>>> {
    let mut sri_buf = [0u8; 32];
    let sri_bytes = postcard::to_slice(&sri, &mut sri_buf)
        .map_err(|_e| ApiError::Serde("serializing the SRI to look up"))?;

    let mut buf = [0u8; N];
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
        ENOENT => Ok(None),
        ENOMEM => Err(ApiError::BufferTooSmall),
        len if len >= 0 => {
            #[allow(clippy::cast_sign_loss)]
            let bytes = buf.get(..len as usize).ok_or(ApiError::BufferTooSmall)?;
            let name = core::str::from_utf8(bytes)
                .map_err(|_e| ApiError::Serde("cell name is not UTF-8"))?;
            let mut out = heapless::String::new();
            out.push_str(name).map_err(|_e| ApiError::BufferTooSmall)?;
            Ok(Some(out))
        }
        code => Err(code.into()),
    }
}
