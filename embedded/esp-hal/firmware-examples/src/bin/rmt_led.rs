//! Drive the devkit's RGB LED over RMT, keeping the whole firmware.
//!
//! `Board` only hands out what the shipped subsystems need. For anything else
//! — RMT, SPI, I2C, LEDC — ask for `Peripherals` next to `&mut Board`: the
//! macro moves the board's parts out of it and leaves you the rest. Reaching
//! for a field the board did claim is a "use of moved value" error at compile
//! time, not a conflict at runtime.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_firmware::Board;
use esp_firmware::esp_hal::Blocking;
use esp_firmware::esp_hal::gpio::Level;
use esp_firmware::esp_hal::peripherals::Peripherals;
use esp_firmware::esp_hal::rmt::{Channel, PulseCode, Rmt, Tx, TxChannelConfig, TxChannelCreator};
use esp_firmware::esp_hal::time::Rate;

/// GPIO8 is the WS2812 RGB LED on the ESP32-C6-DevKitC-1.
const LED: usize = 8;

#[esp_firmware::main]
async fn setup(board: &mut Board, peripherals: Peripherals, spawner: Spawner) {
    // Nothing on the board uses RMT, so it is still here.
    let rmt = Rmt::new(peripherals.RMT, Rate::from_mhz(80)).unwrap();

    // Taking the pin keeps the cell off it, and a `Flex` is a valid RMT output.
    let pin = board.take_pin(LED).unwrap();
    let channel = rmt
        .channel0
        .configure_tx(&TxChannelConfig::default().with_clk_divider(1))
        .unwrap()
        .with_pin(pin);

    spawner.spawn(cycle(channel).unwrap());
}

/// Red, green, blue: half a second on, half a second off.
#[embassy_executor::task]
async fn cycle(mut channel: Channel<'static, Blocking, Tx>) {
    // The onboard LED is bright; keep it easy on the eyes.
    const DIM: u8 = 0x20;
    let colours = [[DIM, 0, 0], [0, DIM, 0], [0, 0, DIM]];
    loop {
        for rgb in colours {
            channel = show(channel, rgb);
            Timer::after(Duration::from_millis(500)).await;
            channel = show(channel, [0; 3]);
            Timer::after(Duration::from_millis(500)).await;
        }
    }
}

/// WS2812B timing in 12.5 ns ticks (the RMT clock set in `setup`, undivided):
/// a `0` is 0.4 µs high then 0.85 µs low, a `1` is 0.8 µs high then 0.45 µs
/// low, and a long low latches the frame — 300 µs here, which the V5 parts want.
const ZERO: PulseCode = PulseCode::new(Level::High, 32, Level::Low, 68);
const ONE: PulseCode = PulseCode::new(Level::High, 64, Level::Low, 36);
const RESET: PulseCode = PulseCode::new(Level::Low, 12_000, Level::Low, 12_000);

/// Clocks one pixel out and hands the channel back once it is on the wire.
fn show(
    channel: Channel<'static, Blocking, Tx>,
    [r, g, b]: [u8; 3],
) -> Channel<'static, Blocking, Tx> {
    // 24 data bits, the reset and the end marker fit in one RMT memory block.
    let mut frame = [PulseCode::end_marker(); 26];
    // The WS2812 shifts green in first, most significant bit first.
    for (byte, codes) in [g, r, b].into_iter().zip(frame.chunks_exact_mut(8)) {
        for (bit, code) in codes.iter_mut().enumerate() {
            *code = if byte & (0x80 >> bit) == 0 { ZERO } else { ONE };
        }
    }
    frame[24] = RESET;
    channel.transmit(&frame).unwrap().wait().unwrap()
}
