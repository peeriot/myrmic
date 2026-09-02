//! Myrmic firmware for ESP32 chips, as a library.
//!
//! Brings up a swarm node — WiFi, a zenoh session, the db client, the hardware
//! watchdogs, and a host for a WASM cell — from your own binary, so you keep
//! the parts you want and replace the ones you don't.
//!
//! # The rule
//!
//! **Ownership decides what runs.** [`Board`] holds the peripherals the
//! shipped subsystems need; [`start`] brings up only what is still on the board
//! when it runs. Taking a peripheral is therefore both "don't run yours" and
//! "give me the hardware" in one move, and the borrow checker enforces that we
//! are not still driving something you took.
//!
//! | Take | And the firmware stops |
//! | ---- | ---------------------- |
//! | [`take_ble`](Board::take_ble) | the NimBLE host and its HCI transport |
//! | [`take_wifi`](Board::take_wifi) | the network service (WiFi, zenoh, db) |
//! | [`take_wasm_host`](Board::take_wasm_host) | the WAMR thread and module storage |
//! | [`take_watchdog`](Board::take_watchdog) | arming the MWDT/RWDT and the heartbeat |
//! | [`take_pin`](Board::take_pin) | offering that GPIO to the cell |
//!
//! # A firmware
//!
//! ```ignore
//! #![no_std]
//! #![no_main]
//!
//! use esp_firmware::{Board, Network};
//!
//! #[esp_firmware::main]
//! async fn setup(board: &mut Board, net: Network, spawner: Spawner) {
//!     // The onboard LED is ours now; the cell can no longer drive GPIO8.
//!     let led = board.take_pin(8).unwrap();
//!     spawner.spawn(blink(led)).unwrap();
//!
//!     // Our BLE stack, not the shipped one.
//!     let bt = board.take_ble().unwrap();
//!     my_stack::run(spawner, bt);
//! }
//! ```
//!
//! Everything still on the board when `setup` returns is started for you.
//!
//! # What your crate still owns
//!
//! `#![no_std]`, `#![no_main]`, a `build.rs` calling
//! `esp_firmware_build::configure()`, and chip features (`esp32c5`, `esp32c6`,
//! `esp32c61`) that forward to this crate — the GPIO map is selected by the
//! feature set of the crate that calls [`board!`], and the build script reads
//! the chip from the same place, so they cannot be inherited. `esp-firmware` is
//! the only dependency [`macro@main`] needs; reach [`esp_hal`], [`esp_rtos`]
//! and [`embassy_executor`] through its re-exports so you can never end up
//! with two versions.

#![no_std]

extern crate alloc;

mod board;
mod cell;
mod config;
mod net;
#[cfg(feature = "stack-hwm")]
mod stack_hwm;
mod wasm;

#[cfg(feature = "ble")]
mod ble;

pub use board::Board;
pub use cell::{Cell, Message, Network, RegisterError, Registration, SendError};
pub use config::Config;

/// Marks the firmware entry point.
///
/// Runs the boot sequence — logger, heap, watchdog boot report, RTOS start —
/// then calls your function with whatever it asks for, and finally [`start`]s
/// what is left on the [`Board`].
///
/// Your function may take any of `&mut Board`, [`Network`] and
/// `embassy_executor::Spawner`, in any order:
///
/// ```ignore
/// #[esp_firmware::main]
/// async fn setup(board: &mut Board) { /* ... */ }
/// ```
///
/// Take `esp_hal::peripherals::Peripherals` as the first argument instead and
/// nothing is claimed for you: the boot sequence still runs, but building the
/// [`Board`] and calling [`start`] are yours. That is the form for a board
/// that must claim its own hardware first — a Signal Layer pipeline, say:
///
/// ```ignore
/// #[esp_firmware::main]
/// async fn setup(mut peripherals: Peripherals, spawner: Spawner) {
///     let pins = pipeline_pins!(peripherals);
///     let board = esp_firmware::board!(peripherals, pins = pins);
///     esp_firmware::start(board, spawner);
/// }
/// ```
pub use esp_firmware_macros::main;

pub use embassy_executor;
pub use esp_hal;
pub use esp_rtos;
pub use wasm_runtime::Pins;
pub use wasm_storage::PartitionLayout;

/// Re-exports the exported macros expand into. Not a stable surface.
#[doc(hidden)]
pub mod __reexports {
    pub use esp_common;
    pub use esp_hal;
    pub use esp_rtos;
    pub use log;
    pub use wasm_runtime;
}

use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use esp_common::esp_watchdog::{liveness, watchdog};
use wasm_runtime::WasmTransfer;
use wasm_runtime::async_request::timer_manager_task;
use wasm_runtime::async_request::zenoh::{Requests, Responses};
use wasm_runtime::async_request::{DbClientRequest, DbClientResponse};
use wasm_storage::WasmStorage;

/// Channels wiring the network service to the WASM host and to [`Network`].
///
/// Plain statics so the compiler enforces `Sync`: they are shared across the
/// main and net-service executor threads, and a cell type (unconditionally
/// `Sync`) would skip that check.
static WASM_TRANSFER: Channel<CriticalSectionRawMutex, WasmTransfer, 1> = Channel::new();
static DB_REQUESTS: Channel<CriticalSectionRawMutex, DbClientRequest, 1> = Channel::new();
static DB_RESPONSES: Channel<CriticalSectionRawMutex, DbClientResponse, 1> = Channel::new();
static ZENOH_REQUESTS: Requests = Channel::new();
static ZENOH_RESPONSES: Responses = Channel::new();

/// The node's handle on the swarm, available before the radio is up.
///
/// Cheap to obtain and safe to hold from boot: it addresses the channels above,
/// which the network service drains once it comes up.
#[must_use]
pub fn network() -> Network {
    Network::new(DB_REQUESTS.sender(), DB_RESPONSES.receiver())
}

/// Boots everything still on the `board`.
///
/// Called for you by [`macro@main`] unless your setup function took
/// `Peripherals`, in which case it is yours to call once you have built the
/// board.
pub fn start(board: Board, spawner: Spawner) {
    let parts = board.into_parts();
    let config = parts.config;

    #[cfg(feature = "ble")]
    if let Some(bt) = parts.bt {
        ble::start(bt, &config);
    }
    // Without the `ble` feature there is no stack to hand it to; the
    // peripheral simply goes unused unless the firmware took it.
    #[cfg(not(feature = "ble"))]
    let _ = parts.bt;

    if let Some(wifi) = parts.wifi {
        net::start_thread(wifi, &config);
    } else {
        log::warn!("[esp-firmware] no WiFi: this node joins no swarm");
    }

    // The WASM host runs unless the firmware claimed the cell slot itself.
    let wasm_host = parts.wasm_host.filter(|_| !cell::slot_is_claimed());

    if wasm_host.is_some() {
        // The request handler bridges the WAMR thread to the db, zenoh, GPIO
        // and timers. It also owns the db channel pair — which is why a native
        // `Cell` may only exist when the WASM host does not.
        spawner.spawn(
            wasm::wasm_request_handler(
                parts.pins,
                DB_REQUESTS.sender(),
                DB_RESPONSES.receiver(),
                ZENOH_REQUESTS.sender(),
                ZENOH_RESPONSES.receiver(),
            )
            .unwrap(),
        );
    }

    spawner.spawn(stats().unwrap());

    if let Some((timg1, rtc)) = parts.watchdog {
        // From here on a wedged runtime resets the device.
        watchdog::arm(
            esp_hal::rtc_cntl::Rtc::new(rtc).rwdt,
            esp_hal::timer::timg::TimerGroup::new(timg1).wdt,
        );
        spawner.spawn(watchdog_feeder(watchdog::feed).unwrap());
    } else {
        log::warn!("[esp-firmware] no watchdog: a wedged runtime will hang, not reset");
    }

    // The WASM host goes last: its thread is the lowest priority in the system,
    // so it must not be scheduled before the rest is in place.
    if let Some((flash, mmu)) = wasm_host {
        let (module_queue, cell_message_queue) =
            wasm_runtime::start_runtime_thread(config.wasm_priority, config.wasm_stack);
        spawner.spawn(wasm::cell_task(cell_message_queue).unwrap());

        let storage = WasmStorage::new(
            mmu,
            esp_common::esp_storage::FlashStorage::new(flash),
            parts.partition_layout,
        );
        spawner
            .spawn(wasm::runtime_handler(storage, WASM_TRANSFER.receiver(), module_queue).unwrap());

        spawner.spawn(timer_manager_task().unwrap());
        #[cfg(feature = "ble")]
        spawner.spawn(wasm_runtime::async_request::ble_manager_task().unwrap());
    } else if cell::slot_is_claimed() {
        log::info!("[esp-firmware] cell slot claimed natively; WASM host not started");
    } else {
        log::info!("[esp-firmware] no WASM host: nothing can be deployed here");
    }
}

/// Task wrapper for the watchdog feeder. Tasks live here rather than in
/// `esp-watchdog` so the embassy-executor version stays a firmware-side choice.
#[embassy_executor::task]
async fn watchdog_feeder(feed: fn()) {
    liveness::watchdog_feeder(feed).await;
}

/// Task wrapper for the stats heartbeat — the sole required liveness task.
#[embassy_executor::task]
async fn stats() {
    #[cfg(feature = "wdt-selftest")]
    let wedge_mode = Some(wasm_runtime::wdt_selftest_wedge_mode as fn() -> u8);
    #[cfg(not(feature = "wdt-selftest"))]
    let wedge_mode = None;

    #[cfg(feature = "stack-hwm")]
    let stack_hwm = Some(stack_hwm::stack_high_water_mark as fn() -> usize);
    #[cfg(not(feature = "stack-hwm"))]
    let stack_hwm = None;

    liveness::heartbeat(wedge_mode, stack_hwm).await;
}

/// Boot steps that must precede `esp_rtos::start`. Not a stable surface —
/// call it only through [`macro@main`].
#[doc(hidden)]
pub fn __boot_early() {
    esp_common::esp_watchdog::report::snapshot("post-init");

    // If this boot follows a watchdog reset, log it and queue the swarm report
    // (SDS Area D). Must run before `watchdog::arm`.
    watchdog::report_boot();
}

/// Boot steps that need the RTOS running. Not a stable surface — call it only
/// through [`macro@main`].
#[doc(hidden)]
pub fn __boot_late() {
    // Main thread shares priority 1 with the network service so the two
    // time-slice.
    esp_rtos::CurrentThreadHandle::set_priority(esp_rtos::CurrentThreadHandle::get(), 1);

    // Paint the unused main stack so peak usage can be measured. Must run
    // after `esp_rtos::start` and before the deep tasks are spawned.
    #[cfg(feature = "stack-hwm")]
    stack_hwm::paint_unused_stack();
}
