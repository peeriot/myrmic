#![allow(unused_imports, dead_code, unused_variables)]
use signal_layer_core::{
    ProcessingStep, EventSlot, Metric, OutletEntry, OutletRegistry, RetainedSlot, Signal,
    SlotEntry, TapError, TapRegistry, Timestamp,
};
use signal_layer_types::{DriverHealth, HealthEvent};
use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_embedded_hal::shared_bus::asynch::spi::SpiDevice;
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Ticker};
use static_cell::StaticCell;
use esp_hal::gpio::{Level, Output};
use esp_hal::i2c::master::I2c;
use esp_hal::spi::master::Spi;
use esp_hal::Async;
pub static TAP__SIGNAL_LAYER_HEALTH: EventSlot<HealthEvent> = EventSlot::new();
pub static TAP_TEMPERATURE: RetainedSlot<f32, Metric> = RetainedSlot::new();
pub static TAP_HUMIDITY: RetainedSlot<f32, Metric> = RetainedSlot::new();
pub static TAP_AVG_TEMPERATURE: RetainedSlot<f32, Metric> = RetainedSlot::new();
pub struct BoardPeripherals {
    pub i2c0: esp_hal::i2c::master::I2c<'static, esp_hal::Async>,
    pub relay1_out: esp_hal::gpio::Flex<'static>,
}
impl BoardPeripherals {
    pub fn new(
        i2c0: esp_hal::peripherals::I2C0<'static>,
        i2c0_scl: esp_hal::peripherals::GPIO10<'static>,
        i2c0_sda: esp_hal::peripherals::GPIO11<'static>,
        relay1_out_gpio: esp_hal::peripherals::GPIO2<'static>,
    ) -> Self {
        let i2c0 = esp_hal::i2c::master::I2c::new(
                i2c0,
                esp_hal::i2c::master::Config::default()
                    .with_frequency(esp_hal::time::Rate::from_khz(400u32)),
            )
            .unwrap()
            .with_scl(i2c0_scl)
            .with_sda(i2c0_sda)
            .into_async();
        let relay1_out = esp_hal::gpio::Flex::new(relay1_out_gpio);
        BoardPeripherals {
            i2c0,
            relay1_out,
        }
    }
}
/// Build [`BoardPeripherals`] from the chip `Peripherals`, moving only the
/// bus peripherals this pipeline uses so the caller keeps ownership of the
/// rest. Pin/peripheral selection lives here (driven by the board manifest),
/// not in the firmware — callers just write `pipeline_board_peripherals!(p)`.
#[macro_export]
macro_rules! pipeline_board_peripherals {
    ($p:ident) => {
        $crate::pipeline_config::BoardPeripherals::new($p .I2C0, $p .GPIO10, $p .GPIO11,
        $p .GPIO2,)
    };
}
static I2C0_BUS: StaticCell<
    Mutex<NoopRawMutex, esp_hal::i2c::master::I2c<'static, esp_hal::Async>>,
> = StaticCell::new();
#[embassy_executor::task]
async fn bme280_task(
    mut bus: I2cDevice<
        'static,
        NoopRawMutex,
        esp_hal::i2c::master::I2c<'static, esp_hal::Async>,
    >,
) {
    let cfg = bme280_driver::Bme280Config {
        i2c_addr: 118u8,
        osrs_t: bme280_driver::Oversampling::X1,
        osrs_p: bme280_driver::Oversampling::X1,
        osrs_h: bme280_driver::Oversampling::X1,
        filter: bme280_driver::Filter::Off,
        t_sb: bme280_driver::Standby::Ms0_5,
    };
    let mut driver = bme280_driver::Bme280::new(&cfg);
    let mut avg_temp_node = moving_average::MovingAverageState::new(moving_average::MovingAverageConfig {
        window: 4usize,
    });
    let mut health = DriverHealth::Up;
    let mut ready = false;
    let mut ticker = Ticker::every(Duration::from_millis(1000u64));
    loop {
        ticker.next().await;
        if !ready {
            match driver.init(&mut bus).await {
                Ok(()) => ready = true,
                Err(_e) => {
                    if health != DriverHealth::Down {
                        health = DriverHealth::Down;
                        TAP__SIGNAL_LAYER_HEALTH
                            .emit(HealthEvent {
                                source: 0u8,
                                state: DriverHealth::Down,
                            });
                        log::error!("[{}] init failed — sensor Down", "bme280");
                    }
                    continue;
                }
            }
        }
        match driver.sample(&mut bus).await {
            Ok(readings) => {
                if health != DriverHealth::Up {
                    health = DriverHealth::Up;
                    TAP__SIGNAL_LAYER_HEALTH
                        .emit(HealthEvent {
                            source: 0u8,
                            state: DriverHealth::Up,
                        });
                    log::info!("[{}] recovered — sensor Up", "bme280");
                }
                let ts = Timestamp(embassy_time::Instant::now().as_millis());
                TAP_TEMPERATURE.update(ts, readings.temperature);
                TAP_HUMIDITY.update(ts, readings.humidity);
                let avg_temp_out = avg_temp_node.step(readings.temperature);
                if let Some(v) = avg_temp_out {
                    TAP_AVG_TEMPERATURE.update(ts, v);
                }
            }
            Err(_e) => {
                ready = false;
                if health == DriverHealth::Up {
                    health = DriverHealth::Degraded;
                    TAP_TEMPERATURE.clear();
                    TAP_HUMIDITY.clear();
                    TAP_AVG_TEMPERATURE.clear();
                    TAP__SIGNAL_LAYER_HEALTH
                        .emit(HealthEvent {
                            source: 0u8,
                            state: DriverHealth::Degraded,
                        });
                    log::warn!("[{}] sample error — sensor Degraded", "bme280");
                }
            }
        }
    }
}
pub fn spawn_sources(spawner: &Spawner, peripherals: BoardPeripherals) {
    let i2c0_mutex = I2C0_BUS
        .init(
            Mutex::<
                NoopRawMutex,
                esp_hal::i2c::master::I2c<'static, esp_hal::Async>,
            >::new(peripherals.i2c0),
        );
    spawner
        .spawn(
            bme280_task(I2cDevice::new(i2c0_mutex)).expect("failed to spawn source task"),
        );
}
/// Register every pipeline tap into the host tap registry by name.
pub fn register_taps(registry: &mut TapRegistry) -> Result<(), TapError> {
    registry
        .register("_signal_layer_health", SlotEntry::event(&TAP__SIGNAL_LAYER_HEALTH))?;
    registry.register("temperature", SlotEntry::retained(&TAP_TEMPERATURE))?;
    registry.register("humidity", SlotEntry::retained(&TAP_HUMIDITY))?;
    registry.register("avg_temperature", SlotEntry::retained(&TAP_AVG_TEMPERATURE))?;
    Ok(())
}
/// Build the tap registry, register all pipeline taps, and hand it to
/// the WASM runtime. Returns the number of taps registered.
/// Called exactly once from the firmware entry point before WASM starts.
pub fn setup_tap_registry() -> usize {
    let mut registry = TapRegistry::new();
    register_taps(&mut registry).expect("tap registry full");
    let count = registry.len();
    wasm_runtime::init_tap_registry(registry);
    count
}
/// Register every pipeline outlet into the host outlet registry by name.
pub fn register_outlets(registry: &mut OutletRegistry) -> Result<(), TapError> {
    Ok(())
}
/// Build the outlet registry, register all pipeline outlets, and hand it
/// to the WASM runtime. Returns the number of outlets registered.
/// Called exactly once from the firmware entry point before WASM starts.
pub fn setup_outlet_registry() -> usize {
    let mut registry = OutletRegistry::new();
    register_outlets(&mut registry).expect("outlet registry full");
    let count = registry.len();
    wasm_runtime::init_outlet_registry(registry);
    count
}
/// Build a [`wasm_runtime::Pins`] set from the manifest's
/// `gpios.general_purpose` list, minus any pins claimed by the
/// Signal Layer (bus pins or `device.pins`). Generated from the board
/// manifest; firmware calls this in place of
/// `wasm_runtime::pins_from_peripherals!` when the Signal Layer is
/// active.
///
/// Each exposed GPIO is moved out of `$p`, so the borrow checker
/// will reject any later use of `$p.GPIOn` for those pins. Reserved
/// pins are emitted as `None` and remain available on `$p` for use
/// by `pipeline_board_peripherals!`. Pins not listed in
/// `general_purpose` are also `None` — letting a board hide chip
/// pins that aren't physically broken out.
#[macro_export]
macro_rules! pipeline_pins {
    ($p:ident) => {
        wasm_runtime::Pins([Some(esp_hal::gpio::Flex::new($p .GPIO0)),
        Some(esp_hal::gpio::Flex::new($p .GPIO1)), None, None, None, None, None, None,
        None, None, None, None, None, None, None, None, None, None, None, None, None,
        None, None, None, None, None, None, None])
    };
}
