//! The shipped BLE stack: the HCI transport thread and the NimBLE host.
//!
//! Started only while the [`Board`](crate::Board) still holds the BT
//! peripheral. Take it and this module does nothing — the peripheral is yours
//! to drive with whatever stack you like.

use core::ffi::c_void;

use esp_common::esp_nimble_host::{self, HostTransport};
use esp_common::esp_radio::ble::controller::BleConnector;
use esp_common::esp_radio_rtos_driver;
use esp_hal::peripherals::BT;
use esp_rtos::embassy::Executor;
use static_cell::StaticCell;
use wasm_runtime::{from_thread_arg, into_thread_arg};

use crate::Config;

/// Spawns the HCI transport thread and the NimBLE host thread.
///
/// The transport runs at the highest priority in the system: it is what drains
/// controller-to-host HCI packets, and packets that are not drained leak their
/// buffers.
pub fn start(bt: BT<'static>, config: &Config) {
    // SAFETY: `transport_thread` is a valid `extern "C"` entry point; its
    // argument is the `into_thread_arg` of the `BT` whose ownership moves to
    // that thread, and start-up runs this once.
    unsafe {
        esp_radio_rtos_driver::task_create(
            "HCI transport",
            transport_thread,
            into_thread_arg(bt),
            config.ble_hci_priority,
            None,
            config.ble_hci_stack,
        );
    }
    // SAFETY: `esp_nimble_host::host_task` is a valid `extern "C"` entry point
    // taking no argument (null arg), and start-up runs this once.
    unsafe {
        esp_radio_rtos_driver::task_create(
            "BLE Host",
            esp_nimble_host::host_task,
            core::ptr::null_mut(),
            config.ble_host_priority,
            None,
            config.ble_host_stack,
        );
    }
}

/// HCI transport thread — bridges BLE controller ↔ NimBLE host.
extern "C" fn transport_thread(arg: *mut c_void) {
    static EXECUTOR: StaticCell<Executor> = StaticCell::new();

    // SAFETY: `arg` is the `into_thread_arg` of the `BT` `start` handed over;
    // the thread runs once and takes ownership.
    let bluetooth: BT<'static> = unsafe { from_thread_arg(arg) };

    let executor = EXECUTOR.init(Executor::new());
    executor.run(|spawner| {
        let controller_connector = BleConnector::new(bluetooth, Default::default()).unwrap();
        let host_transport = HostTransport::new(controller_connector);
        spawner.spawn(transport_tx().unwrap());
        spawner.spawn(transport_rx(host_transport).unwrap());
    });
}

#[embassy_executor::task]
async fn transport_tx() {
    esp_nimble_host::transport_task_tx().await;
}

#[embassy_executor::task]
async fn transport_rx(transport: HostTransport) {
    esp_nimble_host::transport_task_rx(transport).await;
}
