//! BLE Adapter for the Govee H5075 Thermo-Hygrometer
//!
//! Like the SwitchBot CO2 Sensor Pro, and unlike the RuuviTag Pro or SensorPush HTP.xw adapters,
//! this sensor never needs a GATT connection: temperature, humidity and battery are broadcast in
//! the device's own advertisement. Sending the `enable` command starts a scan filtered to Govee's
//! company identifier; each matching advertisement arrives as an `on_device_found` invocation,
//! which decodes and publishes the reading directly, then keeps scanning for the next one.
//!
//! The H5075 puts everything in the Manufacturer Data of its primary advertisement, so a
//! [`ScanMode::Passive`] scan is enough. That is the difference from the SwitchBot adapter, which
//! needs an active scan because the byte identifying its model sits in the scan response, and a
//! passive scan never requests one.
//!
//! Govee's company identifier is shared across its lineup, so an advertisement from another Govee
//! product can reach the handler. The decoder rejects anything whose values fall outside the
//! sensor's operating range rather than publishing a nonsense reading.
//!
//! The scan handle is the only long-lived host resource this adapter holds. It lives in a
//! [`InMemory`], which keeps it across handler invocations without ever writing it to the data
//! layer, so `disable` can stop it. Nothing this adapter holds is persisted: after a restart the
//! cell is idle again and waits for another `enable`.
//!
//! To run this cell on ESP32-C5/ESP32-C61 using myrmic:
//! ```shell
//! $ myrmic build --platform riscv32imac
//! INFO  Attempting to build: examples/ble/adapter-govee-h5075/Cargo.toml
//!    Compiling myrmic-common v0.1.0
//!    ...
//!    Compiling adapter-govee-h5075 v0.1.0
//!     Finished `release` profile [optimized] target(s) in 3.70s
//! Compile success, file examples/target/adapter_govee_h5075.aot was generated.
//! $ myrmic runtime start -d
//! $ myrmic deploy -t embedded
//! $ myrmic send <sri> enable
//! INFO  successfully sent command
//! ```
#![no_std] // required for wasm32-unknown-unknown

/// The BLE device protocol for parsing data
mod protocol;

use myrmic_sdk::ble::{self, DiscoveredDevice, ScanHandle};
use myrmic_sdk::{
    Callback, DiscoveryFilter, InMemory, JsonValue, Metadata, Result, ScanMode, error, info,
    publish,
};

use crate::protocol::parse_measurement;

/// Govee company identifier (Shenzhen Intellirocks Tech), advertised by every H5075. The bytes on
/// the wire are `88 EC`, which read as a little-endian `u16` is `0xEC88`.
const GOVEE_MANUFACTURER_ID: u16 = 0xEC88;

/// The active scan, empty until one is started. Dropping the handle does not stop the scan, so it
/// is kept here across handler invocations until `disable` takes it back out.
static SCAN: InMemory<Option<ScanHandle>> = InMemory::empty();

/// Initializes the cell
#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    info!("Govee H5075 BLE adapter loaded; send `enable` to start scanning")?;

    Ok(())
}

/// Starts scanning for advertisements.
#[myrmic_sdk::cmd]
fn enable(_md: Metadata) -> Result<()> {
    info!("BLE adapter enabled")?;
    start_scanning()
}

/// Stops the scan, if one is running.
#[myrmic_sdk::cmd]
fn disable(_md: Metadata) -> Result<()> {
    if let Some(scan) = SCAN.with(Option::take)? {
        scan.stop()?;
    }

    info!("BLE Adapter disabled")?;

    Ok(())
}

/// Fired for every advertisement matching the filter.
///
/// The scan is left running, so this is the steady-state path rather than a one-off: each new
/// advertisement from any nearby H5075 produces another reading.
#[myrmic_sdk::cmd]
fn on_device_found(_md: Metadata, device: DiscoveredDevice) -> Result<()> {
    let advertisement = &device.advertisement;

    let Some(manufacturer_data) = &advertisement.manufacturer_data else {
        return Ok(());
    };

    match parse_measurement(&manufacturer_data.payload) {
        Ok(measurement) => {
            info!(
                "Received measurement from {}:\n{measurement}",
                device.address
            )?;
            // Publish temperature - this is just to show what we can do. In reality we can
            // publish all of the data we receive.
            publish(
                "govee_temperature",
                &JsonValue::from(measurement.temperature_c),
            )?;
        }
        Err(err) => error!("Failed to parse Govee advertisement: {err}")?,
    }

    Ok(())
}

/// Starts a filtered scan and keeps the handle in the in-memory context.
fn start_scanning() -> Result<()> {
    info!("Scanning for Govee H5075")?;
    let filter = DiscoveryFilter {
        company_id: Some(GOVEE_MANUFACTURER_ID),
        local_name: None,
        service_uuid: None,
    };
    // Everything the sensor reports is in the primary advertisement, so there is no scan response
    // to request.
    let scan = ble::scan(
        Callback::of::<on_device_found>(),
        Some(filter),
        ScanMode::Passive,
    )?;

    SCAN.with(|slot| *slot = Some(scan))?;

    Ok(())
}
