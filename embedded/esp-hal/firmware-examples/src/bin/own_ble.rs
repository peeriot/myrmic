//! Bring your own BLE stack, and keep the onboard LED.
//!
//! Taking the BT peripheral off the board is the whole gesture: the shipped
//! NimBLE host and its HCI transport are never started, and the peripheral is
//! yours. Everything else — WiFi, zenoh, the db client, the watchdog, the WASM
//! host — comes up exactly as it would have.
//!
//! The cell-facing BLE host functions go with the stack, so a cell deployed
//! here can no longer reach BLE unless you reimplement them.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_firmware::Board;
use esp_firmware::esp_hal::gpio::Flex;
use esp_firmware::esp_hal::peripherals::BT;

/// GPIO8 is the onboard LED on the ESP32-C6 devkit.
const LED: usize = 8;

#[esp_firmware::main]
async fn setup(board: &mut Board, spawner: Spawner) {
    if let Some(bt) = board.take_ble() {
        spawner.spawn(my_ble_stack(bt).unwrap());
    }

    // The cell can no longer drive this pin — `Pins` reports it unavailable.
    if let Some(led) = board.take_pin(LED) {
        spawner.spawn(blink(led).unwrap());
    }
}

/// Whatever host stack you like: trouble-host, your own HCI driver, or nothing
/// at all if you only wanted the radio quiet. Owning `bt` for the life of the
/// task is the point — the shipped stack never sees it.
#[embassy_executor::task]
async fn my_ble_stack(bt: BT<'static>) {
    log::info!("BLE is ours now: {bt:?}");
    core::future::pending::<()>().await;
}

#[embassy_executor::task]
async fn blink(mut led: Flex<'static>) {
    led.set_output_enable(true);
    loop {
        led.toggle();
        Timer::after(Duration::from_millis(500)).await;
    }
}
