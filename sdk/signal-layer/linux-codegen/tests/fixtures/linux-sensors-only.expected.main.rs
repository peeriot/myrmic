#![allow(unused_imports, dead_code, unused_variables)]
use signal_layer_core::{
    ProcessingStep, EventSlot, Metric, OutletEntry, OutletRegistry, RetainedSlot, Signal,
    SlotEntry, TapError, TapRegistry, Timestamp,
};
use signal_layer_types::{DriverHealth, HealthEvent};
use tokio::time::{interval, Duration};
use tokio_stream::{StreamExt as _, wrappers::IntervalStream};
/// Linux placeholder: Embassy `Spawner` is not used on Linux.
/// Present so the generated `spawn_sources` signature compiles.
type Spawner = ();
use linux_i2c_shim::{LinuxI2cdev, SharedI2c};
pub static TAP__SIGNAL_LAYER_HEALTH: EventSlot<HealthEvent> = EventSlot::new();
pub static TAP_TEMPERATURE: RetainedSlot<f32, Metric> = RetainedSlot::new();
pub static TAP_HUMIDITY: RetainedSlot<f32, Metric> = RetainedSlot::new();
pub static TAP_LUX: RetainedSlot<f32, Metric> = RetainedSlot::new();
pub static TAP_AVG_TEMPERATURE: RetainedSlot<f32, Metric> = RetainedSlot::new();
pub struct BoardPeripherals {
    pub i2c0: linux_i2c_shim::SharedI2c<linux_i2c_shim::LinuxI2cdev>,
}
impl BoardPeripherals {
    pub fn new() -> Self {
        let i2c0 = {
            let raw = linux_i2c_shim::LinuxI2cdev::open("/dev/i2c-1")
                .expect(concat!("open ", "/dev/i2c-1"));
            linux_i2c_shim::SharedI2c::new(raw)
        };
        BoardPeripherals { i2c0 }
    }
}
impl Default for BoardPeripherals {
    fn default() -> Self {
        Self::new()
    }
}
async fn bme280_task(mut bus: linux_i2c_shim::SharedI2c<linux_i2c_shim::LinuxI2cdev>) {
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
    let mut ticker = IntervalStream::new(interval(Duration::from_millis(1000u64)));
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
                let ts = Timestamp(signal_layer_linux_rt::time::now_millis());
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
async fn veml7700_task(mut bus: linux_i2c_shim::SharedI2c<linux_i2c_shim::LinuxI2cdev>) {
    let cfg = veml7700_driver::Veml7700Config {
        i2c_addr: 16u8,
        gain: veml7700_driver::Gain::X1,
        integration_time: veml7700_driver::IntegrationTime::Ms100,
    };
    let mut driver = veml7700_driver::Veml7700::new(&cfg);
    let mut health = DriverHealth::Up;
    let mut ready = false;
    let mut ticker = IntervalStream::new(interval(Duration::from_millis(2000u64)));
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
                                source: 1u8,
                                state: DriverHealth::Down,
                            });
                        log::error!("[{}] init failed — sensor Down", "veml7700");
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
                            source: 1u8,
                            state: DriverHealth::Up,
                        });
                    log::info!("[{}] recovered — sensor Up", "veml7700");
                }
                let ts = Timestamp(signal_layer_linux_rt::time::now_millis());
                TAP_LUX.update(ts, readings.lux);
            }
            Err(_e) => {
                ready = false;
                if health == DriverHealth::Up {
                    health = DriverHealth::Degraded;
                    TAP_LUX.clear();
                    TAP__SIGNAL_LAYER_HEALTH
                        .emit(HealthEvent {
                            source: 1u8,
                            state: DriverHealth::Degraded,
                        });
                    log::warn!("[{}] sample error — sensor Degraded", "veml7700");
                }
            }
        }
    }
}
pub fn spawn_sources(spawner: &Spawner, peripherals: BoardPeripherals) {
    let i2c0_mutex = peripherals.i2c0;
    tokio::spawn(bme280_task(i2c0_mutex.clone()));
    tokio::spawn(veml7700_task(i2c0_mutex.clone()));
}
/// Register every pipeline tap into the host tap registry by name.
pub fn register_taps(registry: &mut TapRegistry) -> Result<(), TapError> {
    registry
        .register("_signal_layer_health", SlotEntry::event(&TAP__SIGNAL_LAYER_HEALTH))?;
    registry.register("temperature", SlotEntry::retained(&TAP_TEMPERATURE))?;
    registry.register("humidity", SlotEntry::retained(&TAP_HUMIDITY))?;
    registry.register("lux", SlotEntry::retained(&TAP_LUX))?;
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
    struct TapRegistryStore(signal_layer_core::TapRegistry);
    impl signal_layer_ipc::TapStore for TapRegistryStore {
        fn resolve(&self, name: &str) -> Option<u32> {
            self.0.resolve(name)
        }
        fn read_retained(&self, h: u32) -> signal_layer_ipc::StoreRead {
            match self.0.get(h) {
                Some(signal_layer_core::SlotEntry::Retained(r)) => {
                    let mut ts = 0u64;
                    let mut buf = [0u8; 256];
                    match r.read_bytes(&mut ts, &mut buf) {
                        Ok(n) => {
                            signal_layer_ipc::StoreRead::Value {
                                timestamp_ms: ts,
                                bytes: buf[..n].to_vec(),
                            }
                        }
                        Err(signal_layer_core::TapError::Empty) => {
                            signal_layer_ipc::StoreRead::Empty
                        }
                        Err(_) => signal_layer_ipc::StoreRead::InvalidHandle,
                    }
                }
                _ => signal_layer_ipc::StoreRead::InvalidHandle,
            }
        }
        fn take_event(&self, h: u32) -> signal_layer_ipc::StoreRead {
            match self.0.get(h) {
                Some(signal_layer_core::SlotEntry::Event(e)) => {
                    let mut buf = [0u8; 256];
                    match e.take_bytes(&mut buf) {
                        Ok(n) => {
                            signal_layer_ipc::StoreRead::Value {
                                timestamp_ms: 0,
                                bytes: buf[..n].to_vec(),
                            }
                        }
                        Err(signal_layer_core::TapError::Empty) => {
                            signal_layer_ipc::StoreRead::Empty
                        }
                        Err(_) => signal_layer_ipc::StoreRead::InvalidHandle,
                    }
                }
                _ => signal_layer_ipc::StoreRead::InvalidHandle,
            }
        }
        fn list_len(&self) -> u32 {
            self.0.len() as u32
        }
        fn list_entry(&self, index: u32) -> Option<(String, u8)> {
            let name = self.0.name_at(index)?;
            let kind = self.0.get(index)?.kind() as u8;
            Some((name.to_string(), kind))
        }
        fn type_id(&self, h: u32) -> Option<u32> {
            self.0.get(h).map(signal_layer_core::SlotEntry::wire_type_id)
        }
    }
    let socket_path = signal_layer_ipc::default_socket_path()
        .expect(
            "no socket path available: set XDG_RUNTIME_DIR or ensure /run/peeriot is writable",
        );
    let _server = tokio::spawn(
        signal_layer_linux_rt::run_signal_server(
            socket_path,
            std::sync::Arc::new(TapRegistryStore(registry)),
            signal_layer_linux_rt::take_outlet_store(),
        ),
    );
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
    struct OutletRegistryStore(signal_layer_core::OutletRegistry);
    impl signal_layer_ipc::OutletStore for OutletRegistryStore {
        fn resolve(&self, name: &str) -> Option<u32> {
            self.0.resolve(name)
        }
        fn write(&self, h: u32, bytes: &[u8]) -> signal_layer_ipc::StoreWrite {
            let Some(outlet) = self.0.get(h) else {
                return signal_layer_ipc::StoreWrite::InvalidHandle;
            };
            let ts = signal_layer_core::Timestamp(
                signal_layer_linux_rt::time::now_millis(),
            );
            match outlet.write_bytes(ts, bytes) {
                Ok(()) => signal_layer_ipc::StoreWrite::Ok,
                Err(signal_layer_core::TapError::Decode) => {
                    signal_layer_ipc::StoreWrite::Rejected
                }
                Err(_) => signal_layer_ipc::StoreWrite::InvalidHandle,
            }
        }
        fn list_len(&self) -> u32 {
            self.0.len() as u32
        }
        fn list_entry(&self, index: u32) -> Option<(String, u8)> {
            let name = self.0.name_at(index)?;
            let kind = self.0.get(index)?.kind() as u8;
            Some((name.to_string(), kind))
        }
        fn type_id(&self, h: u32) -> Option<u32> {
            self.0.get(h).map(signal_layer_core::OutletEntry::wire_type_id)
        }
    }
    signal_layer_linux_rt::set_outlet_store(
        std::sync::Arc::new(OutletRegistryStore(registry)),
    );
    count
}

#[tokio::main]
async fn main() {
    // Logging: RUST_LOG-controlled, defaulting to `info` so driver health
    // transitions (Up / Degraded / Down) and sample errors are visible.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    log::info!("pipeline `linux-sensors` starting");

    // Set up outlets, then taps — `setup_outlet_registry` parks the outlet
    // store that `setup_tap_registry` hands to the IPC server it starts.
    setup_outlet_registry();
    setup_tap_registry();
    let peripherals = BoardPeripherals::new();
    spawn_sources(&(), peripherals);

    // Run until interrupted.
    println!("Pipeline `linux-sensors` running. Press Ctrl-C to stop.");
    log::info!("pipeline `linux-sensors` running");
    tokio::signal::ctrl_c()
        .await
        .expect("failed to listen for ctrl-c");
}
