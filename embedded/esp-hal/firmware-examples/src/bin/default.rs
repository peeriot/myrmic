//! The whole firmware, unmodified.
//!
//! Joins the swarm and hosts a WASM cell. This is what `modem-esp32` is.

#![no_std]
#![no_main]

#[esp_firmware::main]
async fn setup() {}
