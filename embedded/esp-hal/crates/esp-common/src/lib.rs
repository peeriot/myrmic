//! Facade over the workspace dependencies the ESP firmware uses directly, so a
//! firmware crate pins one workspace-controlled dependency instead of each of
//! them (requested for easier firmware generation).
//!
//! Re-exports only. A firmware keeps as direct dependencies just what its own
//! code names — for the esp-codegen-generated pipeline module that is
//! `esp-hal`, `wasm-runtime` and the signal-layer infrastructure.

#![no_std]

pub use {
    cell_db_service, embassy_futures, embassy_net, esp_alloc, esp_backtrace,
    esp_bootloader_esp_idf, esp_heap, esp_mmu, esp_network, esp_println, esp_radio,
    esp_radio_rtos_driver, esp_storage, esp_watchdog, wasm_storage,
};

#[cfg(feature = "ble")]
pub use esp_nimble_host;
