//! Host functions for the callback-oriented `ble` import module.
//!
//! Each registration operation (`scan`, `connect`, `subscribe`, `read`,
//! `write`) takes a postcard-serialized request from `myrmic_common::types::ble`,
//! drives the per-cell BLE backend, and returns synchronously: `>= 0` = an id (or
//! success), `< 0` = a negative errno. Results arrive later as an ordinary call
//! to the `command_<callback>` export named in the request.

use wasmtime::Caller;

use myrmic_common::types::ble::{
    ConnectRequest, ReadRequest, ScanRequest, SubscribeRequest, WriteRequest,
};

use crate::wasm::cell::state::CellState;
use crate::wasm::host_functions::decode;

pub(crate) async fn scan(mut caller: Caller<'_, CellState>, ptr: u32, len: u32) -> i32 {
    let request: ScanRequest = match decode(&mut caller, ptr, len, "ble scan request") {
        Ok(request) => request,
        Err(code) => return code,
    };

    caller.data().ble().clone().scan(request).await
}

pub(crate) async fn stop_scan(caller: Caller<'_, CellState>) -> i32 {
    caller.data().ble().clone().stop_scan().await
}

pub(crate) async fn connect(mut caller: Caller<'_, CellState>, ptr: u32, len: u32) -> i32 {
    let request: ConnectRequest = match decode(&mut caller, ptr, len, "ble connect request") {
        Ok(request) => request,
        Err(code) => return code,
    };

    caller.data().ble().clone().connect(request).await
}

pub(crate) async fn disconnect(caller: Caller<'_, CellState>, id: u32) -> i32 {
    caller.data().ble().clone().disconnect(id).await
}

pub(crate) async fn subscribe(mut caller: Caller<'_, CellState>, ptr: u32, len: u32) -> i32 {
    let request: SubscribeRequest = match decode(&mut caller, ptr, len, "ble subscribe request") {
        Ok(request) => request,
        Err(code) => return code,
    };

    caller.data().ble().clone().subscribe(request).await
}

pub(crate) async fn unsubscribe(caller: Caller<'_, CellState>, id: u32) -> i32 {
    caller.data().ble().clone().unsubscribe(id).await
}

pub(crate) async fn read(mut caller: Caller<'_, CellState>, ptr: u32, len: u32) -> i32 {
    let request: ReadRequest = match decode(&mut caller, ptr, len, "ble read request") {
        Ok(request) => request,
        Err(code) => return code,
    };

    caller.data().ble().clone().read(request).await
}

pub(crate) async fn write(mut caller: Caller<'_, CellState>, ptr: u32, len: u32) -> i32 {
    let request: WriteRequest = match decode(&mut caller, ptr, len, "ble write request") {
        Ok(request) => request,
        Err(code) => return code,
    };

    caller.data().ble().clone().write(request).await
}

pub(crate) async fn set_pair_passkey(caller: Caller<'_, CellState>, passkey: u32) -> i32 {
    caller.data().ble().clone().set_pair_passkey(passkey).await
}
