//! Runtime host functions: the effective tag set and the runtime id of the
//! device hosting this cell.

use alloc::boxed::Box;
use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;
use core::pin::Pin;

use cell_protocol::RuntimeId;
use myrmic_sdk::{ENOMEM, GENERIC_ERROR, SUCCESS};
use wamr_rust_sdk::sys;
use wamr_rust_sdk::sys::NativeSymbol;

use crate::Error;
use crate::async_request::db::DbClient;
use crate::async_request::send_request_and_wait;
use crate::async_request::zenoh::Zenoh;
use crate::macros::{host_function, host_function_decl};

/// Sets up the runtime imports
#[expect(
    clippy::box_collection,
    reason = "Need to be able to pin from the beginning of the declaration"
)]
pub(crate) fn setup() -> Result<Pin<Box<Vec<NativeSymbol>>>, Error> {
    let native_symbols = Box::pin(vec![
        host_function_decl!(runtime_tags_len_host, c"()i"), // () -> i32
        host_function_decl!(runtime_tags_host, c"(*~)i"),   // (ptr + len) -> i32
        host_function_decl!(runtime_id_host, c"(*~)i"),     // (ptr + len) -> i32
    ]);

    // safety: C FFI
    let success = unsafe {
        sys::wasm_runtime_register_natives(
            c"runtime".as_ptr(),
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
fn runtime_tags_len_host() -> i32 {
    let tags = send_request_and_wait(DbClient::RuntimeTags);
    match postcard::to_allocvec(&tags) {
        Ok(bytes) => i32::try_from(bytes.len()).unwrap_or(GENERIC_ERROR),
        Err(_) => GENERIC_ERROR,
    }
}

#[host_function]
fn runtime_tags_host(buffer: *mut u8, length: i32) -> i32 {
    let tags = send_request_and_wait(DbClient::RuntimeTags);
    let Ok(encoded) = postcard::to_allocvec(&tags) else {
        return GENERIC_ERROR;
    };

    write_bytes(buffer, length, &encoded)
}

#[host_function]
fn runtime_id_host(buffer: *mut u8, length: i32) -> i32 {
    let id = RuntimeId::from(send_request_and_wait(Zenoh::Zid)).to_string();
    let Ok(encoded) = postcard::to_allocvec(&id) else {
        return GENERIC_ERROR;
    };

    write_bytes(buffer, length, &encoded)
}

/// Copies `encoded` into the guest buffer, returning [`SUCCESS`], or an error
/// code ([`ENOMEM`] when the buffer is too small). The guest decodes the
/// length-delimited payload without a written-length hint, so only
/// success/failure is reported.
fn write_bytes(buffer: *mut u8, length: i32, encoded: &[u8]) -> i32 {
    if buffer.is_null() {
        log::info!("buffer pointer is null");
        return GENERIC_ERROR;
    }

    let Ok(max_length) = usize::try_from(length) else {
        return GENERIC_ERROR;
    };

    if encoded.len() > max_length {
        return ENOMEM;
    }

    // safety: buffer is non-null and encoded.len() <= max_length
    let dest = unsafe { core::slice::from_raw_parts_mut(buffer, encoded.len()) };
    dest.copy_from_slice(encoded);

    SUCCESS
}
