//! BLE host functions (callback-oriented ABI)
#![expect(
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "WAMR host functions: WASM i32 ptr/len args are reinterpreted as usize, and small ids as i32"
)]

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::pin::Pin;
use core::slice;

use myrmic_common::types::ble::{
    ConnectRequest, ReadRequest, ScanRequest, SubscribeRequest, WriteRequest,
};
use myrmic_sdk::{GENERIC_ERROR, SUCCESS};
use wamr_rust_sdk::sys;
use wamr_rust_sdk::sys::NativeSymbol;

use crate::Error;
use crate::async_request::ble::Ble;
use crate::async_request::send_request_and_wait;
use crate::macros::{host_function, host_function_decl};

/// Sets up the BLE imports
#[allow(clippy::box_collection)]
pub(super) fn setup() -> Result<Pin<Box<Vec<NativeSymbol>>>, Error> {
    let native_symbols = Box::pin(vec![
        host_function_decl!(ble_scan, c"(*~)i"), // (request, len) -> i32
        host_function_decl!(ble_stop_scan, c"()i"), // () -> i32
        host_function_decl!(ble_connect, c"(*~)i"), // (request, len) -> i32
        host_function_decl!(ble_disconnect, c"(i)i"), // (id) -> i32
        host_function_decl!(ble_subscribe, c"(*~)i"), // (request, len) -> i32
        host_function_decl!(ble_unsubscribe, c"(i)i"), // (id) -> i32
        host_function_decl!(ble_read, c"(*~)i"), // (request, len) -> i32
        host_function_decl!(ble_write, c"(*~)i"), // (request, len) -> i32
        host_function_decl!(ble_set_pair_passkey, c"(i)i"), // (passkey) -> i32
    ]);

    // safety: C FFI
    let success = unsafe {
        sys::wasm_runtime_register_natives(
            c"ble".as_ptr(),
            native_symbols.as_ptr().cast_mut(),
            native_symbols.len() as u32,
        )
    };

    if success {
        log::info!(
            "[wasm] {} symbol(s) registered for module 'ble'",
            native_symbols.len()
        );
        Ok(native_symbols)
    } else {
        Err(Error::Import)
    }
}

#[host_function]
fn ble_scan(request: *const u8, len: i32) -> i32 {
    // SAFETY: WAMR validates the (ptr, len) pair against guest linear memory before dispatch.
    let bytes = unsafe { slice::from_raw_parts(request, len as usize) };
    let Ok(request) = postcard::from_bytes::<ScanRequest>(bytes) else {
        log::error!("Failed to deserialize ScanRequest");

        return GENERIC_ERROR;
    };

    match send_request_and_wait(Ble::Scan {
        // `None` filter means "report every advertisement"; the default filter
        // (all fields unset) matches everything.
        filter: request.filter.unwrap_or_default(),
        callback: request.callback,
        mode: request.mode,
    }) {
        Ok(()) => SUCCESS,
        Err(err) => {
            log::error!("BLE scan failed: {err}");

            GENERIC_ERROR
        }
    }
}

#[host_function]
fn ble_stop_scan() -> i32 {
    match send_request_and_wait(Ble::StopScan) {
        Ok(()) => SUCCESS,
        Err(err) => {
            log::error!("BLE stop_scan failed: {err}");

            GENERIC_ERROR
        }
    }
}

#[host_function]
fn ble_connect(request: *const u8, len: i32) -> i32 {
    // SAFETY: WAMR validates the (ptr, len) pair against guest linear memory before dispatch.
    let bytes = unsafe { slice::from_raw_parts(request, len as usize) };
    let Ok(request) = postcard::from_bytes::<ConnectRequest>(bytes) else {
        log::error!("Failed to deserialize ConnectRequest");

        return GENERIC_ERROR;
    };

    match send_request_and_wait(Ble::Connect {
        address: request.address,
        on_connected: request.on_connected,
        on_disconnected: request.on_disconnected,
    }) {
        Ok(()) => SUCCESS,
        Err(err) => {
            log::error!("BLE connect failed: {err}");

            GENERIC_ERROR
        }
    }
}

#[host_function]
fn ble_disconnect(id: i32) -> i32 {
    match send_request_and_wait(Ble::Disconnect {
        id: id.cast_unsigned(),
    }) {
        Ok(()) => SUCCESS,
        Err(err) => {
            log::error!("BLE disconnect failed: {err}");

            GENERIC_ERROR
        }
    }
}

#[host_function]
fn ble_subscribe(request: *const u8, len: i32) -> i32 {
    // SAFETY: WAMR validates the (ptr, len) pair against guest linear memory before dispatch.
    let bytes = unsafe { slice::from_raw_parts(request, len as usize) };
    let Ok(request) = postcard::from_bytes::<SubscribeRequest>(bytes) else {
        log::error!("Failed to deserialize SubscribeRequest");

        return GENERIC_ERROR;
    };

    match send_request_and_wait(Ble::CharSubscribe {
        connection_id: request.connection_id,
        characteristic: request.characteristic,
        callback: request.callback,
    }) {
        Ok(id) => id as i32,
        Err(err) => {
            log::error!("BLE subscribe failed: {err}");

            GENERIC_ERROR
        }
    }
}

#[host_function]
fn ble_unsubscribe(id: i32) -> i32 {
    match send_request_and_wait(Ble::CharUnsubscribe {
        id: id.cast_unsigned(),
    }) {
        Ok(()) => SUCCESS,
        Err(err) => {
            log::error!("BLE unsubscribe failed: {err}");

            GENERIC_ERROR
        }
    }
}

#[host_function]
fn ble_read(request: *const u8, len: i32) -> i32 {
    // SAFETY: WAMR validates the (ptr, len) pair against guest linear memory before dispatch.
    let bytes = unsafe { slice::from_raw_parts(request, len as usize) };
    let Ok(request) = postcard::from_bytes::<ReadRequest>(bytes) else {
        log::error!("Failed to deserialize ReadRequest");

        return GENERIC_ERROR;
    };

    match send_request_and_wait(Ble::CharRead {
        connection_id: request.connection_id,
        characteristic: request.characteristic,
        callback: request.callback,
    }) {
        Ok(()) => SUCCESS,
        Err(err) => {
            log::error!("BLE read failed: {err}");

            GENERIC_ERROR
        }
    }
}

#[host_function]
fn ble_write(request: *const u8, len: i32) -> i32 {
    // SAFETY: WAMR validates the (ptr, len) pair against guest linear memory before dispatch.
    let bytes = unsafe { slice::from_raw_parts(request, len as usize) };
    let Ok(request) = postcard::from_bytes::<WriteRequest>(bytes) else {
        log::error!("Failed to deserialize WriteRequest");

        return GENERIC_ERROR;
    };

    match send_request_and_wait(Ble::CharWrite {
        connection_id: request.connection_id,
        characteristic: request.characteristic,
        data: request.data,
        callback: request.callback,
    }) {
        Ok(()) => SUCCESS,
        Err(err) => {
            log::error!("BLE write failed: {err}");

            GENERIC_ERROR
        }
    }
}

#[host_function]
fn ble_set_pair_passkey(passkey: i32) -> i32 {
    match send_request_and_wait(Ble::SetPairPasskey {
        passkey: passkey.cast_unsigned(),
    }) {
        Ok(()) => SUCCESS,
        Err(err) => {
            log::error!("BLE set_pair_passkey failed: {err}");

            GENERIC_ERROR
        }
    }
}
