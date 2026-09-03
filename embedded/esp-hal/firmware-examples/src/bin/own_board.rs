//! Build and start the board yourself.
//!
//! `Peripherals` on its own claims nothing for you: the boot sequence runs,
//! then building the `Board` and calling `esp_firmware::start` are yours. This
//! is the shape a Signal Layer pipeline uses, because its generated macros must
//! claim bus peripherals and pins *before* `board!` decides what the cell may
//! have.
//!
//! For a spare peripheral on an otherwise stock board you do not need this:
//! ask for `Peripherals` next to `&mut Board` instead — see `rmt_led`.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use esp_firmware::esp_hal::peripherals::Peripherals;

#[esp_firmware::main]
async fn setup(peripherals: Peripherals, spawner: Spawner) {
    // Ours before the board exists. Claiming moves a field out; `board!` then
    // takes what it needs from what is left.
    let spi = peripherals.SPI2;
    log::info!("SPI2 is ours: {spi:?}");

    let board = esp_firmware::board!(peripherals);
    esp_firmware::start(board, spawner);
}
