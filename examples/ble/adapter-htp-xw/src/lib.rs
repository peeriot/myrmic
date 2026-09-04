//! BLE Adapter for SensorPush HTP.xw sensor
//!
//! This adapter scans, connects and polls the sensor for temperature, humidity and barometric
//! pressure. The HTP.xw does not automatically push measurements: a fresh sample is obtained by
//! writing a trigger value to a measurement characteristic and then reading it back.
//! Sending the `enable` command starts a scan; from there each stage of the flow (device found,
//! connected, trigger written, value read, disconnected) arrives as a command invocation on one of
//! the handlers below. Once connected, a periodic timer drives `sample` every three seconds (can be
//! changed via the [`SAMPLE_PERIOD`] const).
//!
//! The measurement protocol follows the [SensorPush Bluetooth API]: every measurement characteristic
//! returns a little-endian signed 32-bit integer in hundredths of the measured unit.
//!
//! The long-lived host resources (scan, connection, polling timer) are held through handles kept in
//! a single [`InMemory`], so a later invocation can stop, read, or tear them down. An `InMemory`
//! never reaches the data layer: it holds the handles for as long as the cell instance is loaded,
//! which is exactly as long as the resources behind them are valid. Nothing this adapter holds is
//! persisted: after a restart the cell is idle again and waits for another `enable`.
//!
//! To run this cell on ESP32-C5/ESP32-C61 using myrmic:
//! ```shell
//! $ myrmic build --platform riscv32imac
//! INFO  Attempting to build: examples/ble/adapter-htp-xw/Cargo.toml
//!    Compiling myrmic-common v0.1.0
//!    ...
//!    Compiling adapter-htp-xw v0.1.0
//!     Finished `release` profile [optimized] target(s) in 3.70s
//! Create AoT compiler with:
//!   target:        riscv32
//!   target cpu:    generic-rv32
//!   target triple: riscv32-pc-linux-ilp32
//!   cpu features:  +i,+m,+a,+c
//!   opt level:     3
//!   size level:    3
//!   output format: AoT file
//! Compile success, file examples/target/adapter_htp_xw.aot was generated.
//! $ myrmic runtime start -d
//! INFO  runtime "default" pid file: /run/user/1001/myrmic/default.pid
//! $ myrmic deploy -t embedded
//! $ myrmic send <sri> enable
//! INFO  trace ID = 4e6fb5dce99f3be4cdfd749938a5210f
//! INFO  successfully sent command
//! ```
//!
//! [SensorPush Bluetooth API]: https://www.sensorpush.com/bluetooth-api
#![no_std] // required for wasm32-unknown-unknown

/// The BLE device protocol for parsing data
mod protocol;

use core::time::Duration;

use myrmic_sdk::ble::{self, Characteristic, Connection, Disconnect, DiscoveredDevice, ScanHandle};
use myrmic_sdk::types::ble::{ReadOutcome, WriteOutcome};
use myrmic_sdk::{
    Callback, DiscoveryFilter, InMemory, JsonValue, Metadata, Result, ScanMode, TimerHandle, Uuid,
    error, info, interval, publish, uuid128,
};

use crate::protocol::parse_centi_i32;

/// SensorPush manufacturer company identifier, as advertised in its
/// manufacturer-specific data.
const SENSORPUSH_COMPANY_ID: u16 = 0xCD03;
/// SensorPush primary GATT service. Advertised at scan time AND used post-connect
/// to locate the measurement characteristics.
const HTP_SERVICE: Uuid = uuid128!("EF090000-11D6-42BA-93B8-9DD7EC090AB0");
/// Temperature characteristic (hundredths of a degree C, little-endian `i32`).
const TEMPERATURE_CHARACTERISTIC: Uuid = uuid128!("EF090080-11D6-42BA-93B8-9DD7EC090AA9");
/// Relative humidity characteristic (hundredths of a percent, little-endian `i32`).
const HUMIDITY_CHARACTERISTIC: Uuid = uuid128!("EF090081-11D6-42BA-93B8-9DD7EC090AA9");
/// Barometric pressure characteristic (hundredths of a Pascal, little-endian `i32`).
const PRESSURE_CHARACTERISTIC: Uuid = uuid128!("EF090082-11D6-42BA-93B8-9DD7EC090AA9");

/// Trigger written to a measurement characteristic to request a fresh reading.
///
/// The protocol accepts any 32-bit value; each measurement characteristic is triggered
/// separately to refresh its own value before it is read back.
const SAMPLE_TRIGGER: [u8; 4] = [0x01, 0x00, 0x00, 0x00];

/// How often the sensor is polled once connected.
const SAMPLE_PERIOD: Duration = Duration::from_secs(3);

/// The adapter's transient context: the host resources it currently holds. Each is
/// `None` until acquired, and dropping a handle does not release the resource, so
/// they are kept here across handler invocations.
struct AdapterContext {
    /// Active scan, kept so `on_device_found` can stop it once a sensor is found.
    scan: Option<ScanHandle>,
    /// Active connection, kept so it can be polled by `sample` and torn down on `disable`.
    connection: Option<Connection>,
    /// Active polling timer, kept so it can be canceled on disconnect or `disable`.
    timer: Option<TimerHandle>,
}

/// In-memory context, local to this cell instance only.
static CTX: InMemory<AdapterContext> = InMemory::new(AdapterContext {
    scan: None,
    connection: None,
    timer: None,
});

/// Initializes the cell
#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    info!("SensorPush HTP.xw BLE adapter loaded; send `enable` to start scanning")?;

    Ok(())
}

/// Starts scanning for the HTP.xw. Discovery runs on the host; `on_device_found`
/// is invoked for the first advertisement that matches the filter.
#[myrmic_sdk::cmd]
fn enable(_md: Metadata) -> Result<()> {
    info!("BLE adapter enabled")?;
    start_scanning()
}

/// Stops the adapter and releases every host resource it holds.
#[myrmic_sdk::cmd]
fn disable(_md: Metadata) -> Result<()> {
    let mut ctx = CTX.try_borrow_mut()?;
    // Tear each resource
    if let Some(timer) = ctx.timer.take() {
        timer.cancel()?;
    }
    if let Some(scan) = ctx.scan.take() {
        scan.stop()?;
    }
    if let Some(connection) = ctx.connection.take() {
        connection.disconnect()?;
    }

    info!("BLE Adapter disabled")?;

    Ok(())
}

/// Polls the sensor once; invoked on each tick of the polling timer. Each
/// measurement lives in its own characteristic and must be triggered before it
/// holds a fresh value, so the trigger is written to all three; each write
/// response drives `on_trigger_written`, which reads that characteristic back.
#[myrmic_sdk::cmd]
fn sample(_md: Metadata) -> Result<()> {
    let ctx = CTX.try_borrow()?;
    let Some(connection) = ctx.connection.as_ref() else {
        error!("Cannot sample: not connected")?;

        return Ok(());
    };

    for characteristic in [
        temperature_characteristic(connection)?,
        humidity_characteristic(connection)?,
        pressure_characteristic(connection)?,
    ] {
        connection.write(
            characteristic,
            &SAMPLE_TRIGGER,
            Callback::of::<on_trigger_written>(),
        )?;
    }
    info!("Requested a fresh sample")?;

    Ok(())
}

/// The host filtered on company id and service UUID already, so the first hit is our sensor.
#[myrmic_sdk::cmd]
fn on_device_found(_md: Metadata, device: DiscoveredDevice) -> Result<()> {
    let Some(scan) = CTX.with(|ctx| ctx.scan.take())? else {
        // The host may have already queued several advertisements before the scan
        // stops, so this handler can run again after we've committed to a device.
        // Only the first invocation (while the scan handle is still held) acts; once
        // the scan is taken, later stale advertisements find `None` and are ignored.
        return Ok(());
    };

    scan.stop()?;

    info!("Found HTP.xw {}, connecting", device.address)?;
    ble::connect(device.address)
        .on_connected(Callback::of::<on_connected>())
        .on_disconnected(Callback::of::<on_disconnected>())
        .initiate()?;

    Ok(())
}

/// Fired once the link is up and GATT services have been discovered.
#[myrmic_sdk::cmd]
fn on_connected(_md: Metadata, connection: Connection) -> Result<()> {
    info!(
        "Connected to HTP.xw; polling every {} s",
        SAMPLE_PERIOD.as_secs()
    )?;

    // Drive `sample` on a periodic timer for as long as the link is up.
    let timer = interval(Callback::of::<sample>(), SAMPLE_PERIOD).build()?;

    // Keep the connection and timer so `sample`, `disable`, and the disconnect path
    // can act on them.
    CTX.with(|ctx| {
        ctx.connection = Some(connection);
        ctx.timer = Some(timer);
    })?;

    Ok(())
}

/// Fired once a measurement trigger has been written. Reads that same
/// characteristic back; the value is delivered to `on_reading`.
#[myrmic_sdk::cmd]
fn on_trigger_written(_md: Metadata, outcome: WriteOutcome) -> Result<()> {
    if let Err(err) = outcome.result {
        error!(
            "Failed to write sample trigger to {}: {err:?}",
            outcome.characteristic
        )?;

        return Ok(());
    }

    let ctx = CTX.try_borrow()?;
    let Some(connection) = ctx.connection.as_ref() else {
        error!("Trigger written but connection is gone")?;

        return Ok(());
    };

    // Read back the characteristic that was just triggered and refreshed.
    connection.read(outcome.characteristic, Callback::of::<on_reading>())?;

    Ok(())
}

/// Fired for each measurement read. The characteristic that produced the value
/// selects how it is scaled and where it is published.
#[myrmic_sdk::cmd]
fn on_reading(_md: Metadata, outcome: ReadOutcome) -> Result<()> {
    let bytes = match outcome.value {
        Ok(bytes) => bytes,
        Err(err) => {
            error!("Failed to read {}: {err:?}", outcome.characteristic)?;

            return Ok(());
        }
    };

    let value = match parse_centi_i32(&bytes) {
        Ok(value) => value,
        Err(err) => {
            error!("Failed to parse {}: {err}", outcome.characteristic)?;

            return Ok(());
        }
    };

    match outcome.characteristic.uuid {
        TEMPERATURE_CHARACTERISTIC => {
            info!("Temperature: {value:.2} °C")?;
            publish("htp_temperature", &JsonValue::from(value))?;
        }
        HUMIDITY_CHARACTERISTIC => {
            info!("Relative Humidity: {value:.2} %")?;
            publish("htp_humidity", &JsonValue::from(value))?;
        }
        PRESSURE_CHARACTERISTIC => {
            info!("Barometric Pressure: {value:.2} Pa")?;
            publish("htp_pressure", &JsonValue::from(value))?;
        }
        other => error!("Read from unexpected characteristic {other}")?,
    }

    Ok(())
}

/// Fired when the connection is lost (failed to establish, timed out, or dropped).
#[myrmic_sdk::cmd]
fn on_disconnected(_md: Metadata, disconnect: Disconnect) -> Result<()> {
    error!("Disconnected: {disconnect}")?;

    // The host already tore the connection down; drop our copy and stop polling.
    let timer = CTX.with(|ctx| {
        ctx.connection = None;

        ctx.timer.take()
    })?;
    if let Some(timer) = timer {
        timer.cancel()?;
    }

    // Re-arm discovery so the adapter reconnects when the sensor comes back.
    start_scanning()
}

/// Starts a filtered scan and keeps the handle in the in-memory context. Shared by
/// `enable` and the reconnect path in `on_disconnected`.
fn start_scanning() -> Result<()> {
    info!("Scanning for SensorPush HTP.xw")?;
    // Concatenate two advertisement fields: the host reports a device only when
    // BOTH the manufacturer company id and the advertised service UUID match.
    let filter = DiscoveryFilter {
        company_id: Some(SENSORPUSH_COMPANY_ID),
        local_name: None,
        service_uuid: Some(HTP_SERVICE),
    };
    let scan = ble::scan(
        Callback::of::<on_device_found>(),
        Some(filter),
        ScanMode::Passive,
    )?;

    CTX.with(|ctx| ctx.scan = Some(scan))?;

    Ok(())
}

/// Resolves the temperature characteristic on `connection`.
fn temperature_characteristic(connection: &Connection) -> Result<Characteristic> {
    connection
        .characteristic(HTP_SERVICE, TEMPERATURE_CHARACTERISTIC)
        .ok_or("HTP.xw is missing the temperature characteristic")
}

/// Resolves the humidity characteristic on `connection`.
fn humidity_characteristic(connection: &Connection) -> Result<Characteristic> {
    connection
        .characteristic(HTP_SERVICE, HUMIDITY_CHARACTERISTIC)
        .ok_or("HTP.xw is missing the humidity characteristic")
}

/// Resolves the pressure characteristic on `connection`.
fn pressure_characteristic(connection: &Connection) -> Result<Characteristic> {
    connection
        .characteristic(HTP_SERVICE, PRESSURE_CHARACTERISTIC)
        .ok_or("HTP.xw is missing the pressure characteristic")
}
