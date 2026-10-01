#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Ticker};
use esp_firmware::esp_hal::gpio::Flex;
use esp_firmware::esp_hal::peripherals::Peripherals;
use esp_firmware::esp_hal::tsens::{Config, TemperatureSensor};
use esp_firmware::signal_layer_types::DigitalState;
use esp_firmware::{Board, EventTap, Outlet, Tap};

const RELAY: usize = 2;

#[esp_firmware::main]
async fn setup(board: &mut Board, spawner: Spawner, peripherals: Peripherals) {
    let temperature = board.tap::<f32>("temperature").unwrap();
    let relay = board.outlet::<DigitalState>("heat_relay").unwrap();
    let applied = board
        .event_tap::<DigitalState>("heat_relay_applied")
        .unwrap();

    let sensor = TemperatureSensor::new(peripherals.TSENS, Config::default()).unwrap();
    let pin = board.take_pin(RELAY).unwrap();

    spawner.spawn(sample(temperature, sensor).unwrap());
    spawner.spawn(drive(relay, applied, pin).unwrap());
}

#[embassy_executor::task]
async fn sample(tap: Tap<f32>, sensor: TemperatureSensor<'static>) {
    let mut ticker = Ticker::every(Duration::from_secs(1));
    loop {
        ticker.next().await;
        tap.update(sensor.get_temperature().to_celsius());
    }
}

#[embassy_executor::task]
async fn drive(
    outlet: Outlet<DigitalState>,
    applied: EventTap<DigitalState>,
    mut pin: Flex<'static>,
) {
    pin.set_output_enable(true);
    loop {
        let (_at, cmd) = outlet.changed().await;
        pin.set_level(cmd.on.into());
        applied.emit(cmd);
    }
}
