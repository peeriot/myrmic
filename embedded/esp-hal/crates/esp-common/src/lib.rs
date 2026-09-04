//! Facade over the workspace dependencies the ESP firmware uses directly, so a
//! firmware crate pins one workspace-controlled dependency instead of each of
//! them (requested for easier firmware generation).
//!
//! Re-exports only. The firmware keeps as direct dependencies:
//! `embassy-executor` and `esp-rtos` (their proc-macros emit absolute crate
//! paths — and the executor version stays a firmware-side choice), plus the
//! crates the esp-codegen-generated pipeline module names directly (`esp-hal`,
//! `wasm-runtime`, `embassy-sync`, `embassy-time`, `static_cell`, `log` and
//! the signal-layer infrastructure), and `wasm-storage` (the firmware build
//! script emits an absolute path to it).

#![no_std]

pub use {
    cell_db_service, embassy_futures, embassy_net, esp_alloc, esp_backtrace,
    esp_bootloader_esp_idf, esp_heap, esp_mmu, esp_network, esp_println, esp_radio,
    esp_radio_rtos_driver, esp_storage, esp_watchdog, wasm_storage,
};

#[cfg(feature = "ble")]
pub use esp_nimble_host;
