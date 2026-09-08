//! Firmware with a Signal Layer pipeline.
//!
//! The pipeline is described in board.yml (the hardware) and pipeline.yml (the
//! sources, steps and taps), and generated at build time. It claims its bus
//! peripherals and pins first; whatever is left is offered to the cell.

#![no_std]
#![no_main]

use esp_firmware::embassy_executor::Spawner;
use esp_firmware::esp_hal::peripherals::Peripherals;

esp_firmware::pipeline!();

#[esp_firmware::main]
async fn setup(peripherals: Peripherals, spawner: Spawner) {
    let board_peripherals = pipeline_board_peripherals!(peripherals);
    let pins = pipeline_pins!(peripherals);

    let mut board = esp_firmware::board!(peripherals, pins = pins);
    pipeline_config::register_taps(board.taps()).expect("pipeline taps fit the registry");
    pipeline_config::register_outlets(board.outlets()).expect("pipeline outlets fit the registry");
    pipeline_config::spawn_sources(&spawner, board_peripherals);
    esp_firmware::start(board, spawner);
}
