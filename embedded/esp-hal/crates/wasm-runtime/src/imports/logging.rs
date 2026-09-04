//! Logging host functions

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::pin::Pin;

use myrmic_sdk::{GENERIC_ERROR, LogLevel, SUCCESS};
use wamr_rust_sdk::sys;
use wamr_rust_sdk::sys::NativeSymbol;

use crate::Error;
use crate::macros::{host_function, host_function_decl};

/// Sets up the logging imports
#[expect(
    clippy::box_collection,
    reason = "Need to be able to pin from the beginning of the declaration"
)]
pub(crate) fn setup() -> Result<Pin<Box<Vec<NativeSymbol>>>, Error> {
    let native_symbols = Box::pin(vec![
        host_function_decl!(log_host, c"(*~i)i"), // (ptr + len + i32) -> i32
    ]);

    // safety: C FFI
    let success = unsafe {
        sys::wasm_runtime_register_natives(
            c"logging".as_ptr(),
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
fn log_host(buffer: *const u8, length: i32, level: i32) -> i32 {
    if buffer.is_null() {
        log::info!("buffer pointer is null");
        return GENERIC_ERROR;
    }

    #[expect(
        clippy::cast_sign_loss,
        reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
    )]
    // safety: we already checked that buffer is non-null
    let slice = unsafe { core::slice::from_raw_parts(buffer, length as usize) };
    if let Ok(log_msg) = core::str::from_utf8(slice) {
        if let Ok(log_level) = LogLevel::try_from(level) {
            match log_level {
                LogLevel::Trace => log::trace!("module log: {log_msg}"),
                LogLevel::Debug => log::debug!("module log: {log_msg}"),
                LogLevel::Info => log::info!("module log: {log_msg}"),
                LogLevel::Warn => log::warn!("module log: {log_msg}"),
                LogLevel::Error => log::error!("module log: {log_msg}"),
            }
        } else {
            log::warn!("module logged using an invalid log level");
            return GENERIC_ERROR;
        }
    } else {
        log::warn!("module logged using an invalid string");
        return GENERIC_ERROR;
    }

    SUCCESS
}
