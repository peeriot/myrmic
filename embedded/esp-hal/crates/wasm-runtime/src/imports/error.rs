//! Error host functions

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::ffi::c_int;
use core::pin::Pin;

use myrmic_common::types::error::{GENERIC_ERROR, SUCCESS};
use wamr_rust_sdk::sys;
use wamr_rust_sdk::sys::NativeSymbol;
use wasm_runtime_macros::host_function;

use crate::async_request::cell_host::CellHost;
use crate::async_request::send_request_and_wait;
use crate::{Error, host_function_decl};

/// Sets up the error imports
#[expect(
    clippy::box_collection,
    reason = "Need to be able to pin from the beginning of the declaration"
)]
pub(crate) fn setup() -> Result<Pin<Box<Vec<NativeSymbol>>>, Error> {
    let native_symbols = Box::pin(vec![
        host_function_decl!(report_error, c"(*~)i"), // (ptr + len) -> i32
    ]);

    // safety: C FFI
    let success = unsafe {
        sys::wasm_runtime_register_natives(
            c"error".as_ptr(),
            native_symbols.as_ptr().cast_mut(),
            native_symbols.len() as u32,
        )
    };

    if success {
        Ok(native_symbols)
    } else {
        Err(Error::Import)
    }
}

#[host_function]
fn report_error(buffer: *const u8, len: c_int) -> c_int {
    if buffer.is_null() {
        log::info!("buffer pointer is null");
        return GENERIC_ERROR;
    }

    #[expect(
        clippy::cast_sign_loss,
        reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
    )]
    // safety: we already checked that buffer is non-null
    let slice = unsafe { core::slice::from_raw_parts(buffer, len as usize) };
    if let Ok(error_msg) = core::str::from_utf8(slice) {
        send_request_and_wait(CellHost::StoreErrorMessage(error_msg.into()));
        SUCCESS
    } else {
        log::error!("Failed to convert string into error message");
        GENERIC_ERROR
    }
}
