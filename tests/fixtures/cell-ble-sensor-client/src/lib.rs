//! BLE client for the BLE hardware-in-the-loop tests.
//!
//! The HIL rig runs a dedicated BLE sensor next to the device under test. It advertises a fixed
//! name and serves one GATT service with fixed, deterministic contents (the firmware lives in the
//! CI repository). This cell drives the `myrmic_sdk::ble` API against that sensor, one operation
//! per command, and reports every outcome as a `Report` on the `REPORT_EVENT` event. The test
//! harness sends the commands and asserts on the reports, so the cell itself checks nothing.
//!
//! The sensor constants below must match the sensor firmware, and `Report` must match the copy
//! the harness decodes it with: postcard encodes an enum variant by its index, so variants are
//! only ever appended.
//!
//! Commands:
//! - `scan_by_name` / `scan_by_service`: scan with a filter on the advertised name or service
//!   UUID, stop at the first match and report `Report::Found`.
//! - `connect`: scan by name, connect to the first match and report `Report::Connected`.
//! - `read_only` / `read_writable` / `read_counter`: read one characteristic (`Report::Read`).
//! - `write` / `write_no_response`: write the command payload to the writable characteristic.
//!   Only the former reports (`Report::Written`); a write without response has no outcome.
//! - `subscribe` / `unsubscribe`: notifications from the counter characteristic, each reported as
//!   `Report::Notified`.
//! - `disconnect`: drops the link (`Report::DisconnectRequested`). A link that drops on its own,
//!   or never comes up, is reported as `Report::Disconnected`.

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Debug;

use myrmic_sdk::ble::{
    self, Connection, Disconnect, DiscoveredDevice, Notification, ScanHandle, Subscription,
};
use myrmic_sdk::types::ble::{ReadOutcome, WriteOutcome};
use myrmic_sdk::{
    Bytes, Callback, Characteristic, DiscoveryFilter, InMemory, Metadata, Result, ScanMode, Uuid,
    publish, uuid128,
};
use serde::{Deserialize, Serialize};

/// Name the sensor advertises.
const SENSOR_NAME: &str = "hil-ble";
/// The sensor's GATT service, also carried in its advertisement.
const SENSOR_SERVICE: Uuid = uuid128!("7A3E0001-4C1B-4F6E-9C2D-5B8E1F0A7D30");
/// Read-only characteristic with a constant value.
const READ_ONLY: Uuid = uuid128!("7A3E0002-4C1B-4F6E-9C2D-5B8E1F0A7D30");
/// Characteristic that reads back what was last written to it.
const WRITABLE: Uuid = uuid128!("7A3E0003-4C1B-4F6E-9C2D-5B8E1F0A7D30");
/// Characteristic that notifies an incrementing counter.
const COUNTER: Uuid = uuid128!("7A3E0004-4C1B-4F6E-9C2D-5B8E1F0A7D30");

/// Event every outcome is published on.
const REPORT_EVENT: &str = "ble_report";

/// One outcome, as the harness sees it. UUIDs and addresses are carried as their display strings
/// so the harness needs none of the SDK's BLE types to decode a report.
#[derive(Serialize, Deserialize, myrmic_sdk::Message)]
#[codec(myrmic_sdk::Postcard)]
enum Report {
    /// The scan matched the sensor's advertisement.
    Found {
        address: String,
        local_name: Option<String>,
        service_uuids: Vec<String>,
    },
    /// The link is up. Lists which of the sensor's characteristics were discovered.
    Connected { characteristics: Vec<String> },
    /// The outcome of a read.
    Read {
        characteristic: String,
        value: core::result::Result<Vec<u8>, String>,
    },
    /// The outcome of a write with response.
    Written {
        characteristic: String,
        result: core::result::Result<(), String>,
    },
    /// The outcome of `subscribe`.
    Subscribed {
        result: core::result::Result<(), String>,
    },
    /// A notification arrived.
    Notified {
        characteristic: String,
        data: Vec<u8>,
    },
    /// `unsubscribe` went through.
    Unsubscribed,
    /// `disconnect` went through. A disconnect the cell asks for doesn't invoke
    /// `on_disconnected`, so this is the only report it gets.
    DisconnectRequested,
    /// The link failed to come up, or dropped without the cell asking.
    Disconnected { reason: String },
    /// A command could not run, e.g. `read_only` before `connect`.
    Failed { reason: String },
}

/// What scanning is for: report the match, or connect to it.
#[derive(Clone, Copy)]
enum ScanPurpose {
    Report,
    Connect,
}

/// Host resources held across invocations. Dropping a handle doesn't release what it stands for,
/// so each one is kept here until a later command takes it back out.
struct Context {
    scan: Option<(ScanHandle, ScanPurpose)>,
    connection: Option<Connection>,
    subscription: Option<Subscription>,
}

static CTX: InMemory<Context> = InMemory::new(Context {
    scan: None,
    connection: None,
    subscription: None,
});

#[myrmic_sdk::cmd]
fn scan_by_name(_md: Metadata) -> Result<()> {
    start_scan(name_filter()?, ScanPurpose::Report)
}

#[myrmic_sdk::cmd]
fn scan_by_service(_md: Metadata) -> Result<()> {
    let filter = DiscoveryFilter {
        service_uuid: Some(SENSOR_SERVICE),
        ..DiscoveryFilter::default()
    };
    start_scan(filter, ScanPurpose::Report)
}

#[myrmic_sdk::cmd]
fn connect(_md: Metadata) -> Result<()> {
    // Scan first: some backends can't connect to a peripheral they haven't seen advertise.
    start_scan(name_filter()?, ScanPurpose::Connect)
}

#[myrmic_sdk::cmd]
fn read_only(_md: Metadata) -> Result<()> {
    read(READ_ONLY)
}

#[myrmic_sdk::cmd]
fn read_writable(_md: Metadata) -> Result<()> {
    read(WRITABLE)
}

#[myrmic_sdk::cmd]
fn read_counter(_md: Metadata) -> Result<()> {
    read(COUNTER)
}

#[myrmic_sdk::cmd]
fn write(_md: Metadata, data: Bytes) -> Result<()> {
    with_characteristic(WRITABLE, |connection, characteristic| {
        connection.write(characteristic, &data, Callback::of::<on_written>())
    })
}

#[myrmic_sdk::cmd]
fn write_no_response(_md: Metadata, data: Bytes) -> Result<()> {
    with_characteristic(WRITABLE, |connection, characteristic| {
        connection.write_no_response(characteristic, &data)
    })
}

#[myrmic_sdk::cmd]
fn subscribe(_md: Metadata) -> Result<()> {
    let Some(connection) = CTX.with(|ctx| ctx.connection.clone())? else {
        return report(&not_connected());
    };
    let Some(characteristic) = connection.characteristic(SENSOR_SERVICE, COUNTER) else {
        return report(&missing(COUNTER));
    };
    let result = match connection.subscribe(characteristic, Callback::of::<on_notified>()) {
        Ok(Ok(subscription)) => {
            CTX.with(|ctx| ctx.subscription = Some(subscription))?;

            Ok(())
        }
        Ok(Err(e)) => Err(format!("{e:?}")),
        Err(e) => Err(format!("{e:?}")),
    };

    report(&Report::Subscribed { result })
}

#[myrmic_sdk::cmd]
fn unsubscribe(_md: Metadata) -> Result<()> {
    let Some(subscription) = CTX.with(|ctx| ctx.subscription.take())? else {
        return report(&Report::Failed {
            reason: "not subscribed".to_string(),
        });
    };
    if let Err(e) = subscription.unsubscribe() {
        return report(&failed("unsubscribe", e));
    }

    report(&Report::Unsubscribed)
}

#[myrmic_sdk::cmd]
fn disconnect(_md: Metadata) -> Result<()> {
    // The host tears the subscription down with the link, as in `on_disconnected`.
    let connection = CTX.with(|ctx| {
        ctx.subscription = None;

        ctx.connection.take()
    })?;
    let Some(connection) = connection else {
        return report(&not_connected());
    };
    if let Err(e) = connection.disconnect() {
        return report(&failed("disconnect", e));
    }

    report(&Report::DisconnectRequested)
}

#[myrmic_sdk::cmd]
fn on_device_found(_md: Metadata, device: DiscoveredDevice) -> Result<()> {
    // Advertisements the host queued before the scan stopped still arrive here; only the first
    // one, while the handle is still held, counts.
    let Some((scan, purpose)) = CTX.with(|ctx| ctx.scan.take())? else {
        return Ok(());
    };
    if let Err(e) = scan.stop() {
        return report(&failed("stopping the scan", e));
    }

    match purpose {
        ScanPurpose::Report => {
            let advertisement = &device.advertisement;
            report(&Report::Found {
                address: device.address.to_string(),
                local_name: advertisement.local_name.as_ref().map(|n| n.as_str().into()),
                service_uuids: advertisement
                    .service_uuids
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            })
        }
        ScanPurpose::Connect => {
            let initiated = ble::connect(device.address)
                .on_connected(Callback::of::<on_connected>())
                .on_disconnected(Callback::of::<on_disconnected>())
                .initiate();

            initiated.or_else(|e| report(&failed("connect", e)))
        }
    }
}

#[myrmic_sdk::cmd]
fn on_connected(_md: Metadata, connection: Connection) -> Result<()> {
    let characteristics = [READ_ONLY, WRITABLE, COUNTER]
        .into_iter()
        .filter(|uuid| connection.characteristic(SENSOR_SERVICE, *uuid).is_some())
        .map(|uuid| uuid.to_string())
        .collect();
    CTX.with(|ctx| ctx.connection = Some(connection))?;
    report(&Report::Connected { characteristics })
}

#[myrmic_sdk::cmd]
fn on_disconnected(_md: Metadata, disconnect: Disconnect) -> Result<()> {
    // The host tears the subscription down with the link.
    CTX.with(|ctx| {
        ctx.connection = None;
        ctx.subscription = None;
    })?;
    report(&Report::Disconnected {
        reason: format!("{:?}", disconnect.reason()),
    })
}

#[myrmic_sdk::cmd]
fn on_read(_md: Metadata, outcome: ReadOutcome) -> Result<()> {
    report(&Report::Read {
        characteristic: outcome.characteristic.uuid.to_string(),
        value: outcome.value.map_err(|e| format!("{e:?}")),
    })
}

#[myrmic_sdk::cmd]
fn on_written(_md: Metadata, outcome: WriteOutcome) -> Result<()> {
    report(&Report::Written {
        characteristic: outcome.characteristic.uuid.to_string(),
        result: outcome.result.map_err(|e| format!("{e:?}")),
    })
}

#[myrmic_sdk::cmd]
fn on_notified(_md: Metadata, notification: Notification) -> Result<()> {
    report(&Report::Notified {
        characteristic: notification.characteristic().uuid.to_string(),
        data: notification.data().to_vec(),
    })
}

fn name_filter() -> Result<DiscoveryFilter> {
    Ok(DiscoveryFilter {
        local_name: Some(SENSOR_NAME.try_into().map_err(|_| "sensor name too long")?),
        ..DiscoveryFilter::default()
    })
}

/// Starts a scan for `purpose`, or reports why it can't: a failed command is otherwise silent,
/// and the test would only see the discovery timeout.
fn start_scan(filter: DiscoveryFilter, purpose: ScanPurpose) -> Result<()> {
    let scan = match ble::scan(
        Callback::of::<on_device_found>(),
        Some(filter),
        // The sensor puts its name and service UUID in the primary advertisement.
        ScanMode::Passive,
    ) {
        Ok(scan) => scan,
        Err(e) => return report(&failed("starting the scan", e)),
    };
    CTX.with(|ctx| ctx.scan = Some((scan, purpose)))
}

fn read(uuid: Uuid) -> Result<()> {
    with_characteristic(uuid, |connection, characteristic| {
        connection.read(characteristic, Callback::of::<on_read>())
    })
}

/// Runs `op` on the current connection and the sensor characteristic `uuid`, or reports why it
/// can't or why `op` failed.
fn with_characteristic(
    uuid: Uuid,
    op: impl FnOnce(&Connection, Characteristic) -> myrmic_sdk::ApiResult<()>,
) -> Result<()> {
    let Some(connection) = CTX.with(|ctx| ctx.connection.clone())? else {
        return report(&not_connected());
    };
    let Some(characteristic) = connection.characteristic(SENSOR_SERVICE, uuid) else {
        return report(&missing(uuid));
    };

    op(&connection, characteristic).or_else(|e| report(&failed("the operation", e)))
}

fn not_connected() -> Report {
    Report::Failed {
        reason: "not connected".to_string(),
    }
}

/// A host call that failed synchronously. Reported so the test sees the error, not a timeout.
fn failed(action: &str, error: impl Debug) -> Report {
    Report::Failed {
        reason: format!("{action} failed: {error:?}"),
    }
}

fn missing(uuid: Uuid) -> Report {
    Report::Failed {
        reason: format!("sensor service has no characteristic {uuid}"),
    }
}

fn report(report: &Report) -> Result<()> {
    publish(REPORT_EVENT, report)
}
