//! Publish a tap and act on an outlet, with no pipeline.
//!
//! Taps and outlets are the two directions across the cell boundary: a tap is
//! a value the firmware publishes for the cell to read, an outlet a command the
//! cell writes for the firmware to act on. A Signal Layer pipeline declares
//! them from YAML through the codegen; a board with one reading and one relay
//! can declare them here instead, and the cell cannot tell the difference.
//!
//! A cell deployed to this node reads `uptime_s` with
//! `Tap::resolve("uptime_s")?.read_typed::<u32>()`, switches the LED with
//! `Outlet::resolve("led")?.write_typed(&DigitalState { on: true })`, and
//! learns when that took effect from the `led_applied` event tap.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Ticker};
use esp_firmware::esp_hal::gpio::Flex;
use esp_firmware::signal_layer_types::DigitalState;
use esp_firmware::{Board, EventTap, Outlet, Tap};

/// GPIO8 is the onboard LED on the ESP32-C6 devkit.
const LED: usize = 8;

#[esp_firmware::main]
async fn setup(board: &mut Board, spawner: Spawner) {
    let uptime = board.tap::<u32>("uptime_s").unwrap();
    let led = board.outlet::<DigitalState>("led").unwrap();
    let applied = board.event_tap::<DigitalState>("led_applied").unwrap();

    // Ours to drive; the cell reaches it only through the outlet.
    let pin = board.take_pin(LED).unwrap();

    spawner.spawn(count(uptime).unwrap());
    spawner.spawn(drive(led, applied, pin).unwrap());
}

/// Seconds since boot, republished every second.
#[embassy_executor::task]
async fn count(uptime: Tap<u32>) {
    let mut ticker = Ticker::every(Duration::from_secs(1));
    let mut seconds = 0u32;
    loop {
        ticker.next().await;
        seconds += 1;
        uptime.update(seconds);
    }
}

/// Applies each command as it lands, then reports that it did. No polling:
/// `changed` wakes this task from the cell's write.
#[embassy_executor::task]
async fn drive(led: Outlet<DigitalState>, applied: EventTap<DigitalState>, mut pin: Flex<'static>) {
    pin.set_output_enable(true);
    loop {
        let (_at, cmd) = led.changed().await;
        pin.set_level(cmd.on.into());
        applied.emit(cmd);
    }
}
