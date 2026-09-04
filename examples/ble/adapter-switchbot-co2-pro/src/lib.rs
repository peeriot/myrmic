//! BLE Adapter for SwitchBot CO2 Sensor Pro
//!
//! Unlike the RuuviTag Pro or SensorPush HTP.xw adapters, this sensor never needs a GATT
//! connection: every reading (temperature, humidity, CO2, battery) is broadcast passively in the
//! device's own BLE advertisement. Sending the `enable` command starts a scan filtered to
//! SwitchBot's advertised service UUID; each matching advertisement arrives as an `on_device_found`
//! invocation, which decodes and publishes the reading directly, then keeps scanning for the next
//! one.
//!
//! SwitchBot's service UUID and manufacturer id are shared by every product in its lineup (locks,
//! plugs, curtains, other meters...), so the scan filter alone cannot single out the CO2 Sensor
//! Pro: each advertisement is additionally checked against the model character carried in its
//! Service Data (see [`protocol::is_co2_sensor_pro`]) before being decoded.
//!
//! The scan handle is the only long-lived host resource this adapter holds. It lives in a
//! [`InMemory`], which keeps it across handler invocations without ever writing it to the data
//! layer, so `disable` can stop it. Nothing this adapter holds is persisted: after a restart the
//! cell is idle again and waits for another `enable`.
//!
//! To run this cell on ESP32-C5/ESP32-C61 using myrmic:
//! ```shell
//! $ myrmic build --platform riscv32imac
//! INFO  Attempting to build: examples/ble/adapter-switchbot-co2-pro/Cargo.toml
//!    Compiling myrmic-common v0.1.0
//!    ...
//!    Compiling adapter-switchbot-co2-pro v0.1.0
//!     Finished `release` profile [optimized] target(s) in 3.70s
//! Create AoT compiler with:
//!   target:        riscv32
//!   target cpu:    generic-rv32
//!   target triple: riscv32-pc-linux-ilp32
//!   cpu features:  +i,+m,+a,+c
//!   opt level:     3
//!   size level:    3
//!   output format: AoT file
//! Compile success, file examples/target/adapter_switchbot_co2_pro.aot was generated.
//! $ myrmic runtime start -d
//! INFO  runtime "default" pid file: /run/user/1001/myrmic/default.pid
//! $ myrmic deploy -t embedded
//! $ myrmic send <sri> enable
//! INFO  trace ID = 4e6fb5dce99f3be4cdfd749938a5210f
//! INFO  successfully sent command
//! ```
#![no_std] // required for wasm32-unknown-unknown

/// The BLE device protocol for parsing data
mod protocol;

use myrmic_sdk::ble::{self, DiscoveredDevice, ScanHandle};
use myrmic_sdk::{
    Callback, DiscoveryFilter, InMemory, JsonValue, Metadata, Result, ScanMode, Uuid, error, info,
    publish,
};

use crate::protocol::{is_co2_sensor_pro, parse_measurement};

/// SwitchBot's assigned 16-bit service UUID, advertised by every current-generation SwitchBot
/// device.
const SWITCHBOT_SERVICE: Uuid = Uuid::Bit16(0xFD3D);
/// SwitchBot manufacturer company identifier (Woan Technology (Shenzhen) Co., Ltd,
/// used by SwitchBot for all of its devices).
const SWITCHBOT_MANUFACTURER_ID: u16 = 0x0969;

/// The active scan, empty until one is started. Dropping the handle does not stop the scan, so it
/// is kept here across handler invocations until `disable` takes it back out.
static SCAN: InMemory<Option<ScanHandle>> = InMemory::empty();

/// Initializes the cell
#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    info!("SwitchBot CO2 Sensor Pro BLE adapter loaded; send `enable` to start scanning")?;

    Ok(())
}

/// Starts scanning for the CO2 Sensor Pro. Discovery runs on the host; `on_device_found` is
/// invoked for every matching advertisement, for as long as the scan stays active.
#[myrmic_sdk::cmd]
fn enable(_md: Metadata) -> Result<()> {
    info!("BLE adapter enabled")?;

    info!("Scanning for SwitchBot CO2 Sensor Pro")?;
    let filter = DiscoveryFilter {
        company_id: Some(SWITCHBOT_MANUFACTURER_ID),
        local_name: None,
        service_uuid: Some(SWITCHBOT_SERVICE),
    };
    // SwitchBot splits its advertisement across the primary advertisement (manufacturer
    // data) and the scan response (service data, which carries the model byte); a passive
    // scan never requests the scan response, so active scanning is required here.
    let scan_handle = ble::scan(
        Callback::of::<on_device_found>(),
        Some(filter),
        ScanMode::Active,
    )?;

    SCAN.with(|handle| *handle = Some(scan_handle))?;

    Ok(())
}

/// Stops the adapter and releases the scan it holds.
#[myrmic_sdk::cmd]
fn disable(_md: Metadata) -> Result<()> {
    if let Some(scan) = SCAN.with(Option::take)? {
        scan.stop()?;
    }

    info!("BLE Adapter disabled")?;

    Ok(())
}

/// Fired for every advertisement matching the scan filter, i.e. every SwitchBot device in range.
/// The filter narrows by service UUID and manufacturer id alone, which every SwitchBot product
/// shares, so each advertisement is checked against the CO2 Sensor Pro's model character before
/// being decoded; advertisements from other SwitchBot devices are ignored.
#[myrmic_sdk::cmd]
fn on_device_found(_md: Metadata, device: DiscoveredDevice) -> Result<()> {
    let advertisement = &device.advertisement;
    info!("found {advertisement:?}")?;

    let Some(service_data) = &advertisement.service_data else {
        return Ok(());
    };
    if !is_co2_sensor_pro(&service_data.payload) {
        return Ok(());
    }
    let Some(manufacturer_data) = &advertisement.manufacturer_data else {
        return Ok(());
    };

    match parse_measurement(&service_data.payload, &manufacturer_data.payload) {
        Ok(measurement) => {
            info!("Received measurement:\n{measurement}")?;
            // Publish CO2 concentration - this is just to show what we can do. In reality we can
            // publish all of the data we receive.
            if let Some(co2_ppm) = measurement.co2_ppm {
                publish("switchbot_co2_ppm", &JsonValue::from(co2_ppm))?;
            }
        }
        Err(err) => error!("Failed to parse SwitchBot advertisement: {err}")?,
    }

    Ok(())
}
