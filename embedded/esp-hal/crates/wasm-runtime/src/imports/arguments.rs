//! Arguments host function

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::ffi::c_int;
use core::pin::Pin;

use myrmic_common::types::error::{EINVAL, GENERIC_ERROR, SUCCESS};
use wamr_rust_sdk::sys;
use wamr_rust_sdk::sys::NativeSymbol;
use wasm_runtime_macros::host_function;

use crate::async_request::cell_host::CellHost;
use crate::async_request::send_request_and_wait;
use crate::{Error, host_function_decl};

/// Sets up the arguments imports
#[expect(
    clippy::box_collection,
    reason = "Need to be able to pin from the beginning of the declaration"
)]
pub(crate) fn setup() -> Result<Pin<Box<Vec<NativeSymbol>>>, Error> {
    let native_symbols = Box::pin(vec![
        host_function_decl!(get_arguments, c"(*~)i"), // (ptr + len) -> i32
    ]);

    // safety: C FFI
    let success = unsafe {
        sys::wasm_runtime_register_natives(
            c"arguments".as_ptr(),
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
fn get_arguments(buffer: *mut u8, len: c_int) -> c_int {
    if buffer.is_null() {
        log::info!("buffer pointer is null");
        return GENERIC_ERROR;
    }

    let args = send_request_and_wait(CellHost::GetArguments);

    match args {
        #[expect(
            clippy::cast_sign_loss,
            reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
        )]
        Some(data) if data.len() <= len as usize => {
            // safety: we already checked buffer is non-null
            let dest = unsafe { core::slice::from_raw_parts_mut(buffer, data.len()) };
            // panic: we already checked that data fits
            dest.copy_from_slice(data.as_slice());

            #[expect(
                clippy::cast_possible_wrap,
                reason = "We started from an i32, we can cast to i32"
            )]
            (data.len() as c_int)
        }
        None => SUCCESS,
        _ => {
            log::error!("buffer too small");
            EINVAL
        }
    }
}
