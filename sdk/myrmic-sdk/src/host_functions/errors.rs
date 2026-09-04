//! Wrappers for host functions for error reporting beyond logging.

use core::ffi::c_int;

use crate::{ApiResult, error::ErrorCode};

mod c_functions {

    use core::ffi::c_int;

    #[link(wasm_import_module = "error")]
    unsafe extern "C" {

        /// Requests the host to store the provided String as an error message of the calling module
        ///
        /// # Arguments
        /// - buffer: pointer to a buffer containing the string with the error message
        /// - length: the number of bytes that the host should read from the buffer
        ///
        /// # Returns:
        /// - [`crate::SUCCESS`] on success
        /// - [`crate::GENERIC_ERROR`] on failure
        pub(super) fn report_error(buffer: *const u8, length: c_int) -> i32;
    }
}

/// Stores the provided string as an error message in the error queue of the calling module
pub fn report_error(err_msg: &str) -> ApiResult<()> {
    // SAFETY: calling the imported function with pointer and length of guest linear memory
    // used for this specific purpose.
    unsafe { c_functions::report_error(err_msg.as_ptr(), err_msg.len() as c_int) }.to_result()
}
