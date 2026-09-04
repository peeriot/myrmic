//! Network task wrappers.
//!
//! The bodies live in `esp_network`; the tasks stay in the binary so the
//! embassy-executor version remains a firmware-side choice. The session-scoped
//! services (cell DB service, zenoh request adapter) are composed here and
//! handed to the supervisor as a future.

use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Receiver, Sender};
use esp_common::embassy_futures::join::join;
use esp_common::embassy_net::{self, Runner};
use esp_common::esp_radio::wifi::{Interface, WifiController};
use esp_hal::peripherals::WIFI;
use wasm_runtime::WasmTransfer;
use wasm_runtime::async_request::zenoh::{RequestsReceiver, ResponsesSender};
use wasm_runtime::async_request::{DbClientRequest, DbClientResponse};

pub use esp_common::esp_network::CONNECTED;
use esp_common::esp_watchdog::liveness::{Task, bump};

/// Starts the service that establishes and serves the Zenoh communication
///
/// # Panics
///
/// Panics if the WiFi module cannot be initialized
pub fn start_service(
    spawner: Spawner,
    wifi: WIFI<'static>,
    wasm_transfer: Sender<'static, CriticalSectionRawMutex, WasmTransfer, 1>,
    db_requests: Receiver<'static, CriticalSectionRawMutex, DbClientRequest, 1>,
    db_responses: Sender<'static, CriticalSectionRawMutex, DbClientResponse, 1>,
    zenoh_requests: RequestsReceiver,
    zenoh_responses: ResponsesSender,
) {
    let (controller, stack, runner) = esp_common::esp_network::init_stack(wifi);

    spawner.spawn(connection(controller).unwrap());
    spawner.spawn(net_task(runner).unwrap());
    spawner.spawn(
        zenoh_session(
            stack,
            wasm_transfer,
            db_requests,
            db_responses,
            zenoh_requests,
            zenoh_responses,
        )
        .unwrap(),
    );
}

/// Establishes and keeps a WiFi connection
#[embassy_executor::task]
async fn connection(controller: WifiController<'static>) {
    esp_common::esp_network::connection(controller, || bump(Task::Connection)).await;
}

/// Network stack runner
#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, Interface>) {
    runner.run().await;
}

/// Supervisor task: owns the Zenoh session lifecycle and reconnects on peer
/// disconnect. The session-scoped services are composed here.
#[embassy_executor::task]
async fn zenoh_session(
    stack: embassy_net::Stack<'static>,
    wasm_transfer: Sender<'static, CriticalSectionRawMutex, WasmTransfer, 1>,
    db_requests: Receiver<'static, CriticalSectionRawMutex, DbClientRequest, 1>,
    db_responses: Sender<'static, CriticalSectionRawMutex, DbClientResponse, 1>,
    zenoh_requests: RequestsReceiver,
    zenoh_responses: ResponsesSender,
) {
    esp_common::esp_network::zenoh_session(
        stack,
        |session| async move {
            join(
                esp_common::cell_db_service::service(
                    session,
                    wasm_transfer,
                    db_requests,
                    db_responses,
                    esp_common::esp_network::SESSION_LEASE,
                    esp_common::esp_network::wall_time,
                ),
                esp_common::esp_network::zenoh_client(
                    session,
                    zenoh_requests,
                    zenoh_responses,
                    || {
                        bump(Task::ZenohClient);
                    },
                ),
            )
            .await;
        },
        || bump(Task::ZenohSession),
    )
    .await;
}
