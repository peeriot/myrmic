//! Modem firmware for ESP32 chips — the reference `esp-firmware` binary.
//!
//! Everything this firmware does lives in [`esp_firmware`]; what is left here
//! is the crate-level scaffolding a binary must own (`no_std`/`no_main`, the
//! chip features, the build script) and the Signal Layer pipeline wiring, which
//! is code-generated into *this* crate and so cannot live in the library.

#![no_std]
#![no_main]

#[cfg(feature = "pipeline")]
#[rustfmt::skip] // generated artifact — never format, never require the file to exist
mod pipeline_config;

#[cfg(not(feature = "pipeline"))]
#[esp_firmware::main]
async fn setup() {}

// With a pipeline, the board claims its own hardware first: the codegen-emitted
// macros move the bus peripherals named by the board manifest and the pins it
// reserves out of `Peripherals`, and only what is left is offered to the cell.
// The pipeline's slots go into the board's registries, which `start` hands to
// the runtime; the generated `setup_*_registry` entry points are not used.
#[cfg(feature = "pipeline")]
#[esp_firmware::main]
async fn setup(mut peripherals: Peripherals, spawner: Spawner) {
    let board_peripherals = pipeline_board_peripherals!(peripherals);
    let pins = pipeline_pins!(peripherals);

    let mut board = esp_firmware::board!(peripherals, pins = pins);
    pipeline_config::register_taps(board.taps()).expect("pipeline taps fit the registry");
    pipeline_config::register_outlets(board.outlets()).expect("pipeline outlets fit the registry");
    pipeline_config::spawn_sources(&spawner, board_peripherals);
    esp_firmware::start(board, spawner);
}
