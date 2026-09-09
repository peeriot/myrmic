//! The network service: WiFi, the zenoh session, and the session-scoped
//! services (cell db service, zenoh request adapter).
//!
//! The bodies live in `esp_network` and `cell_db_service`; the tasks stay here
//! so the embassy-executor version remains a firmware-side choice. The whole
//! service runs on its own thread: its poll chains are the deepest in the
//! firmware, so they get an owned, fixed-size stack rather than a claim on
//! whatever RAM `.bss` leaves the main stack.

use core::ffi::c_void;
use core::time::Duration;
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Receiver, Sender};
use esp_common::embassy_futures::join::join;
use esp_common::embassy_net::{self, Runner};
use esp_common::esp_radio::wifi::{Interface, WifiController};
use esp_common::esp_radio_rtos_driver;
use esp_hal::peripherals::WIFI;
use esp_rtos::embassy::Executor;
use static_cell::StaticCell;
use wasm_runtime::WasmTransfer;
use wasm_runtime::async_request::zenoh::{RequestsReceiver, ResponsesSender};
use wasm_runtime::async_request::{DbClientRequest, DbClientResponse};
use wasm_runtime::{from_thread_arg, into_thread_arg};

pub use esp_common::esp_network::CONNECTED;
use esp_common::esp_watchdog::liveness::{Task, bump};

use crate::Config;

/// Everything [`service_thread`] hands to [`start_service`].
struct ServiceArgs {
    wifi: WIFI<'static>,
    zenoh: ZenohArgs,
}

struct ZenohArgs {
    wasm_transfer: Sender<'static, CriticalSectionRawMutex, WasmTransfer, 1>,
    db_requests: Receiver<'static, CriticalSectionRawMutex, DbClientRequest, 1>,
    db_responses: Sender<'static, CriticalSectionRawMutex, DbClientResponse, 1>,
    zenoh_requests: RequestsReceiver,
    zenoh_responses: ResponsesSender,
    node_lease_ttl: Duration,
    node_lease_renewal_interval: Duration,
}

/// Starts the network service on its own [`Config::net_stack`]-sized thread.
pub fn start_thread(wifi: WIFI<'static>, config: &Config) {
    let args = ServiceArgs {
        wifi,
        zenoh: ZenohArgs {
            wasm_transfer: crate::WASM_TRANSFER.sender(),
            db_requests: crate::DB_REQUESTS.receiver(),
            db_responses: crate::DB_RESPONSES.sender(),
            zenoh_requests: crate::ZENOH_REQUESTS.receiver(),
            zenoh_responses: crate::ZENOH_RESPONSES.sender(),
            node_lease_ttl: config.node_lease_ttl,
            node_lease_renewal_interval: config.node_lease_renewal_interval,
        },
    };

    // SAFETY: `service_thread` is a valid `extern "C"` entry point; its
    // argument is the `into_thread_arg` of the `ServiceArgs` whose ownership
    // moves to the thread, and start-up runs this once.
    unsafe {
        esp_radio_rtos_driver::task_create(
            "net service",
            service_thread,
            into_thread_arg(args),
            config.net_priority,
            None,
            config.net_stack,
        );
    }
}

extern "C" fn service_thread(arg: *mut c_void) {
    static EXECUTOR: StaticCell<Executor> = StaticCell::new();

    // SAFETY: `arg` is the `into_thread_arg` of the `ServiceArgs` built in
    // `start_thread`; the thread runs once and takes ownership.
    let args: ServiceArgs = unsafe { from_thread_arg(arg) };

    let executor = EXECUTOR.init(Executor::new());
    executor.run(|spawner| {
        start_service(spawner, args);
    });
}

/// Starts the service that establishes and serves the Zenoh communication
///
/// # Panics
///
/// Panics if the WiFi module cannot be initialized
fn start_service(spawner: Spawner, args: ServiceArgs) {
    let ServiceArgs { wifi, zenoh } = args;

    let (controller, stack, runner) = esp_common::esp_network::init_stack(wifi);

    spawner.spawn(connection(controller).unwrap());
    spawner.spawn(net_task(runner).unwrap());
    spawner.spawn(zenoh_session(stack, zenoh).unwrap());
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
async fn zenoh_session(stack: embassy_net::Stack<'static>, zenoh_args: ZenohArgs) {
    let ZenohArgs {
        wasm_transfer,
        db_requests,
        db_responses,
        zenoh_requests,
        zenoh_responses,
        node_lease_ttl,
        node_lease_renewal_interval,
    } = zenoh_args;

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
                    node_lease_ttl,
                    node_lease_renewal_interval,
                    esp_common::esp_network::wall_time,
                    crate::cell::registration().map(|r| esp_common::cell_db_service::NativeCell {
                        sri: r.sri,
                        commands: r.commands,
                        name: r.name,
                        online: crate::cell::mark_online,
                    }),
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
