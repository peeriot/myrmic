use core::ffi::c_int;

use crate::ApiResult;

mod c_functions {
    use core::ffi::c_int;
    #[link(wasm_import_module = "arguments")]
    unsafe extern "C" {
        /// Receiving the command/event payload. This function is used by the module to retrieve the payload
        /// of a command or event that triggered the running of the module.
        ///
        /// # Arguments:
        /// - buffer: pointer to the module memory where the host shall write the payload
        /// - length: maximal length of the payload
        ///
        /// # Returns:
        /// - n of written bytes on success (non-negative number)
        /// - -1 on failure
        pub(super) fn get_arguments(buffer: *mut u8, length: c_int) -> c_int;

    }
}

/// Copies the payload of the command/event that triggered this invocation into
/// `buffer`, returning the number of bytes written.
///
/// Handlers normally receive the payload already decoded — via
/// [`Decoder::from_args`](crate::Decoder::from_args) — instead of calling this.
#[allow(clippy::cast_sign_loss)]
pub fn get_arguments(buffer: &mut [u8]) -> ApiResult<usize> {
    // SAFETY: calling the imported function with pointer and length of guest linear memory
    // used for this specific purpose.
    let status_code =
        unsafe { c_functions::get_arguments(buffer.as_mut_ptr(), buffer.len() as c_int) };
    match status_code {
        n if n >= 0 => Ok(n as usize),
        error_code => Err(error_code.into()),
    }
}
