//! WASM task wrappers.
//!
//! The bodies live in `wasm_runtime` (the `service` module); the tasks stay in
//! the binary so the embassy-executor version remains a firmware-side choice.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Receiver, Sender};
use esp_common::esp_radio_rtos_driver::queue::QueueHandle;
use wasm_runtime::async_request::zenoh::{RequestsSender, ResponsesReceiver};
use wasm_runtime::async_request::{DbClientRequest, DbClientResponse};
use wasm_runtime::{Pins, WasmTransfer};
use wasm_storage::WasmStorage;

use crate::network::CONNECTED;
use esp_common::esp_watchdog::liveness::{Task, bump};

/// Task that handles the system (Hardware and async WASM requests)
#[embassy_executor::task]
pub async fn wasm_request_handler(
    pins: Pins,
    db_requests: Sender<'static, CriticalSectionRawMutex, DbClientRequest, 1>,
    db_responses: Receiver<'static, CriticalSectionRawMutex, DbClientResponse, 1>,
    zenoh_requests: RequestsSender,
    zenoh_responses: ResponsesReceiver,
) {
    // Await connection if we are not already connected
    if !CONNECTED.signaled() {
        CONNECTED.wait().await;
    }
    log::info!("Starting WASM request handler");
    wasm_runtime::async_request::request_handler(
        pins,
        db_requests,
        db_responses,
        zenoh_requests,
        zenoh_responses,
        || bump(Task::RequestHandler),
    )
    .await;
}

/// Gather system's data and provides back-pressure via embassy channels to the RTOS queue
#[embassy_executor::task]
pub async fn cell_task(cell_message_queue: &'static QueueHandle) {
    // Liveness (observed): blocks on an empty cell channel.
    wasm_runtime::cell_pump(cell_message_queue, || bump(Task::Cell)).await;
}

/// Task that is responsible for the dynamic storing/loading/unloading of WASM modules between the
/// storage and the WASM runtime
#[embassy_executor::task]
pub async fn runtime_handler(
    wasm_storage: WasmStorage<CriticalSectionRawMutex>,
    wasm_file_transfer: Receiver<'static, CriticalSectionRawMutex, WasmTransfer, 1>,
    module_queue: &'static QueueHandle,
) {
    // Liveness (observed): blocks awaiting a deployment.
    wasm_runtime::runtime_handler(wasm_storage, wasm_file_transfer, module_queue, || {
        bump(Task::RuntimeHandler);
    })
    .await;
}
