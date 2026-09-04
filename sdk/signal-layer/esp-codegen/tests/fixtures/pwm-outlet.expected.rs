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
pub static OUTLET_FAN1_DUTY: RetainedSlot<signal_layer_types::PwmDuty> = RetainedSlot::new();
static PIPELINE_LEDC: StaticCell<esp_hal::ledc::Ledc<'static>> = StaticCell::new();
static PWM_TIMER_FAN1: StaticCell<
    esp_hal::ledc::timer::Timer<'static, esp_hal::ledc::LowSpeed>,
> = StaticCell::new();
pub struct BoardPeripherals {
    pub i2c0: esp_hal::i2c::master::I2c<'static, esp_hal::Async>,
    pub fan1_out: esp_hal::ledc::channel::Channel<'static, esp_hal::ledc::LowSpeed>,
}
impl BoardPeripherals {
    pub fn new(
        i2c0: esp_hal::peripherals::I2C0<'static>,
        i2c0_scl: esp_hal::peripherals::GPIO10<'static>,
        i2c0_sda: esp_hal::peripherals::GPIO11<'static>,
        fan1_out_gpio: esp_hal::peripherals::GPIO1<'static>,
        ledc: esp_hal::peripherals::LEDC<'static>,
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
        use esp_hal::ledc::channel::ChannelIFace;
        use esp_hal::ledc::timer::TimerIFace;
        let ledc: &'static mut esp_hal::ledc::Ledc<'static> = PIPELINE_LEDC
            .init(esp_hal::ledc::Ledc::new(ledc));
        ledc.set_global_slow_clock(esp_hal::ledc::LSGlobalClkSource::APBClk);
        let fan1_pwm_timer = PWM_TIMER_FAN1
            .init(
                ledc
                    .timer::<
                        esp_hal::ledc::LowSpeed,
                    >(esp_hal::ledc::timer::Number::Timer0),
            );
        fan1_pwm_timer
            .configure(esp_hal::ledc::timer::config::Config {
                duty: esp_hal::ledc::timer::config::Duty::Duty10Bit,
                clock_source: esp_hal::ledc::timer::LSClockSource::APBClk,
                frequency: esp_hal::time::Rate::from_khz(25u32),
            })
            .unwrap();
        let fan1_pwm_timer: &'static esp_hal::ledc::timer::Timer<
            'static,
            esp_hal::ledc::LowSpeed,
        > = fan1_pwm_timer;
        let mut fan1_out = ledc
            .channel::<
                esp_hal::ledc::LowSpeed,
            >(esp_hal::ledc::channel::Number::Channel0, fan1_out_gpio);
        fan1_out
            .configure(esp_hal::ledc::channel::config::Config {
                timer: fan1_pwm_timer,
                duty_pct: 0,
                drive_mode: esp_hal::gpio::DriveMode::PushPull,
            })
            .unwrap();
        BoardPeripherals { i2c0, fan1_out }
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
        $p .GPIO1, $p .LEDC,)
    };
}
#[embassy_executor::task]
async fn fan1_sink_task(
    out: esp_hal::ledc::channel::Channel<'static, esp_hal::ledc::LowSpeed>,
) {
    let cfg = pwm_output_driver::PwmOutputConfig {
        min_duty: 0f32,
        max_duty: 1f32,
        min_update_interval_ms: 0u64,
    };
    let mut driver = pwm_output_driver::PwmOutput::new(&cfg, out);
    if driver.init().is_err() {
        log::error!("[{}] outlet init failed", "fan1");
    }
    let mut last_ts = None;
    let mut ticker = Ticker::every(Duration::from_millis(100u64));
    loop {
        ticker.next().await;
        let now = embassy_time::Instant::now().as_millis();
        let ts = Timestamp(now);
        if let Some((slot_ts, cmd)) = OUTLET_FAN1_DUTY.read() {
            if last_ts != Some(slot_ts) {
                if driver.apply(cmd, now).is_err() {
                    log::error!("[{}] outlet apply failed", "fan1");
                } else {
                    last_ts = Some(slot_ts);
                }
            }
        }
    }
}
pub fn spawn_sources(spawner: &Spawner, peripherals: BoardPeripherals) {
    spawner
        .spawn(fan1_sink_task(peripherals.fan1_out).expect("failed to spawn sink task"));
}
/// Register every pipeline tap into the host tap registry by name.
pub fn register_taps(registry: &mut TapRegistry) -> Result<(), TapError> {
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
    registry.register("fan1_duty", OutletEntry::retained(&OUTLET_FAN1_DUTY))?;
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
        wasm_runtime::Pins([Some(esp_hal::gpio::Flex::new($p .GPIO0)), None,
        Some(esp_hal::gpio::Flex::new($p .GPIO2)), None, None, None, None, None, None,
        None, None, None, None, None, None, None, None, None, None, None, None, None,
        None, None, None, None, None, None])
    };
}
