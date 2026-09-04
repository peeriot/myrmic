//! BLE Adapter for RuuviTag Pro sensor
//!
//! This adapter scans, connects and subscribes to the sensor's heartbeat so that measurement data
//! can be published as an event. Sending the `enable` command starts a scan; from there each stage
//! of the flow (device found, connected, heartbeat notified, disconnected) arrives as a command
//! invocation on one of the handlers below. BLE operations are wired through callbacks that allow
//! to construct the logic of the cell.
//!
//! The long-lived host resources (scan, connection, notification subscription) are held through
//! handles kept in a single [`InMemory`], so a later invocation can stop, read, or tear them
//! down. An `InMemory` never reaches the data layer: it holds the handles for as long as the cell
//! instance is loaded, which is exactly as long as the resources behind them are valid. Nothing
//! this adapter holds is persisted: after a restart the cell is idle again and waits for another
//! `enable`.
//!
//! To run this cell on ESP32-C5/ESP32-C61 using myrmic:
//! ```shell
//! $ myrmic build --platform riscv32imac
//! INFO  Attempting to build: examples/ble/adapter-ruuvitag-pro/Cargo.toml
//!    Compiling myrmic-common v0.1.0
//!    ...
//!    Compiling adapter-ruuvitag-pro v0.1.0
//!     Finished `release` profile [optimized] target(s) in 3.70s
//! Create AoT compiler with:
//!   target:        riscv32
//!   target cpu:    generic-rv32
//!   target triple: riscv32-pc-linux-ilp32
//!   cpu features:  +i,+m,+a,+c
//!   opt level:     3
//!   size level:    3
//!   output format: AoT file
//! Compile success, file examples/target/adapter_ruuvitag_pro.aot was generated.
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

use myrmic_sdk::ble::{
    self, Connection, Disconnect, DiscoveredDevice, Notification, ScanHandle, Subscription,
};
use myrmic_sdk::{
    Callback, DiscoveryFilter, InMemory, JsonValue, Metadata, Result, ScanMode, Uuid, error, info,
    publish, uuid128,
};

use crate::protocol::parse_gatt_heartbeat;

/// Expected Company Identifier (Ruuvi Innovations Ltd.)
const MANUFACTURER_ID: u16 = 0x0499;

/// Nordic NUS Service
const NUS_SERVICE: Uuid = uuid128!("6E400001-B5A3-F393-E0A9-E50E24DCCA9E");
/// Characteristic for TX
///
/// This characteristic is used to receive data from the peripheral
const TX_CHARACTERISTIC: Uuid = uuid128!("6E400003-B5A3-F393-E0A9-E50E24DCCA9E");

/// The adapter's transient context: the host resources it currently holds. Each is
/// `None` until acquired, and dropping a handle does not release the resource, so
/// they are kept here across handler invocations.
struct AdapterContext {
    /// Active scan, kept so `on_device_found` can stop it once a tag is found.
    scan: Option<ScanHandle>,
    /// Active connection, kept so it can be torn down on `disable`.
    connection: Option<Connection>,
    /// Active notification subscription, kept so it can be torn down.
    subscription: Option<Subscription>,
}

/// In-memory context, local to this cell instance only.
static CTX: InMemory<AdapterContext> = InMemory::new(AdapterContext {
    scan: None,
    connection: None,
    subscription: None,
});

/// Initializes the cell
#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    info!("RuuviTag Pro BLE adapter loaded; send `enable` to start scanning")?;

    Ok(())
}

/// Starts scanning for the RuuviTag. Discovery runs on the host; `on_device_found`
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
    if let Some(scan) = ctx.scan.take() {
        scan.stop()?;
    }
    if let Some(subscription) = ctx.subscription.take() {
        subscription.unsubscribe()?;
    }
    if let Some(connection) = ctx.connection.take() {
        connection.disconnect()?;
    }

    info!("BLE Adapter disabled")?;

    Ok(())
}

/// The host filtered on the manufacturer id already, so the first hit is our tag.
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

    info!("Found RuuviTag {}, connecting", device.address)?;
    ble::connect(device.address)
        .on_connected(Callback::of::<on_connected>())
        .on_disconnected(Callback::of::<on_disconnected>())
        .initiate()?;

    Ok(())
}

/// Fired once the link is up and GATT services have been discovered.
#[myrmic_sdk::cmd]
fn on_connected(_md: Metadata, connection: Connection) -> Result<()> {
    info!("Connected; locating NUS TX characteristic")?;

    let tx_characteristic = connection
        .characteristic(NUS_SERVICE, TX_CHARACTERISTIC)
        .ok_or("RuuviTag is missing the NUS TX characteristic")?;

    // Each heartbeat notification is delivered to `on_heartbeat`.
    let subscription = connection
        .subscribe(tx_characteristic, Callback::of::<on_heartbeat>())?
        .map_err(|_| "TX characteristic is not notifiable")?;

    // Persist both handles so `disable` (or a future reconnect) can act on them.
    CTX.with(|ctx| {
        ctx.connection = Some(connection);
        ctx.subscription = Some(subscription);
    })?;

    info!("Subscribed to GATT heartbeat")?;

    Ok(())
}

/// Fired for every heartbeat notification from the tag.
#[myrmic_sdk::cmd]
fn on_heartbeat(_md: Metadata, notification: Notification) -> Result<()> {
    match parse_gatt_heartbeat(notification.data()) {
        Ok(measurement) => {
            info!("Received data:\n{measurement}")?;
            // Publish temperature - this is just to show what we can do. In reality we can publish
            // all of the data we receive
            if let Some(temperature) = measurement.temperature_c {
                publish("ruuvi_temperature", &JsonValue::from(temperature))?;
            }
        }
        Err(_) => error!("Failed to parse GATT heartbeat")?,
    }

    Ok(())
}

/// Fired when the connection is lost (failed to establish, timed out, or dropped).
#[myrmic_sdk::cmd]
fn on_disconnected(_md: Metadata, disconnect: Disconnect) -> Result<()> {
    error!("Disconnected: {disconnect}")?;

    // The host already tore the subscription down with the link; drop our copies.
    CTX.with(|ctx| {
        ctx.connection = None;
        ctx.subscription = None;
    })?;

    // Re-arm discovery so the adapter reconnects when the tag comes back.
    start_scanning()
}

/// Starts a filtered scan and keeps the handle in the in-memory context. Shared by
/// `enable` and the reconnect path in `on_disconnected`.
fn start_scanning() -> Result<()> {
    info!("Scanning for RuuviTag Pro")?;
    let filter = DiscoveryFilter {
        company_id: Some(MANUFACTURER_ID),
        local_name: None,
        service_uuid: None,
    };
    let scan = ble::scan(
        Callback::of::<on_device_found>(),
        Some(filter),
        ScanMode::Passive,
    )?;

    CTX.with(|ctx| ctx.scan = Some(scan))?;

    Ok(())
}
