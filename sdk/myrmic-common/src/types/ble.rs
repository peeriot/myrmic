//! BLE Common types and constants

pub use device::{
    Address, Advertisement, DiscoveredDevice, MAX_ADVERTISED_SERVICE_UUIDS, ManufacturerData,
    SecuritySettings, ServiceData,
};
pub use macros::{parse_mac_str, parse_uuid_str};
pub use uuid::Uuid;

mod device;
mod macros;
mod uuid;

use core::fmt::{Display, Formatter};

#[cfg(feature = "alloc")]
use alloc::collections::BTreeMap;
#[cfg(feature = "alloc")]
use alloc::string::String;
#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

/// Filter passed to `Ble::discover_with_filter`.
///
/// The host applies the filter inside its advertisement-scanning loop and only
/// returns once a matching advertisement arrives, so the WASM cell never has
/// to loop over irrelevant advertisements itself.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiscoveryFilter {
    /// Only return advertisements whose manufacturer-specific data carries this
    /// company identifier (little-endian as stored in `ManufacturerData`).
    pub company_id: Option<u16>,
    /// Only return advertisements whose Complete or Shortened Local Name AD field
    /// matches this string exactly (case-sensitive, up to 32 bytes).
    pub local_name: Option<heapless::String<32>>,
    /// Only return advertisements that include this UUID, either in their advertised
    /// Service UUID list (AD types 0x02 / 0x03 for 16-bit, 0x06 / 0x07 for 128-bit)
    /// or as the UUID of a Service Data element (AD type 0x16).
    pub service_uuid: Option<Uuid>,
}

/// Whether a scan requests scan responses in addition to primary advertisements.
///
/// Some peripherals split their advertised data across the primary advertisement
/// (`ADV_IND`) and the scan response (`SCAN_RSP`) - for example, a device may carry its
/// manufacturer data in the former and its service data in the latter. A passive scan
/// never requests `SCAN_RSP`, so any field a device only advertises there is never seen.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    postcard::experimental::max_size::MaxSize,
)]
pub enum ScanMode {
    /// Only primary advertisements are received. Lower power and radio airtime.
    #[default]
    Passive,
    /// Also sends scan requests and receives scan responses, at the cost of extra power
    /// and radio airtime, so that data split across both is captured.
    Active,
}

/// Characteristic Read Errors
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReadError {
    /// The value is larger than the provided buffer.
    BufTooSmall,
    /// The characteristic does not support reads.
    NotReadable,
    /// The characteristic requires an encrypted/authenticated link to read.
    RequiresSecurity,
}

/// Characteristic Write Errors
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WriteError {
    /// The characteristic does not support writes.
    NotWriteable,
    /// The characteristic requires an encrypted/authenticated link to write.
    RequiresSecurity,
}

/// Characteristic Notify Errors
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotifyError {
    /// The notified value is larger than the provided buffer.
    BufTooSmall,
    /// The characteristic does not support notifications.
    NotNotifiable,
    /// The characteristic requires an encrypted/authenticated link to
    /// subscribe.
    RequiresSecurity,
}

/// A connected BLE Device
#[cfg(feature = "alloc")]
pub struct ConnectedDevice {
    /// MAC address of the device
    pub mac_address: Address,
    /// Discovered GATT services, keyed by service UUID.
    pub gatt_services: BTreeMap<Uuid, Service>,
}

/// A GATT Service: its discovered characteristics, keyed by characteristic UUID.
#[cfg(feature = "alloc")]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct Service {
    pub characteristics: BTreeMap<Uuid, Characteristic>,
}

/// A GATT Characteristic
#[derive(
    Debug,
    Copy,
    Clone,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    postcard::experimental::max_size::MaxSize,
)]
pub struct Characteristic {
    /// UUID of the characteristic itself.
    pub uuid: Uuid,
    /// UUID of the parent service
    pub service_uuid: Uuid,
}

impl Display for Characteristic {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        self.uuid.fmt(f)
    }
}

/// Why a connection ended, or failed to be established.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[allow(missing_docs)] // the #[error] messages document the variants
pub enum DisconnectReason {
    #[error("the connection attempt never succeeded")]
    ConnectionFailed,
    #[error("the connection attempt or an operation timed out.")]
    Timeout,
    #[error("the peripheral closed the connection.")]
    RemoteClosed,
    #[error("the cell requested the disconnect.")]
    LocalClosed,
    #[error("the connection ended for an unspecified reason.")]
    Unknown,
}

/// Request to start a filtered scan.
#[cfg(feature = "alloc")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanRequest {
    /// Name of the command to invoke for each matching advertisement
    ///
    /// # Note
    ///
    /// When scanning is stopped, the host might have more queued advertisements to report, so don't
    /// assume that it is the last.
    pub callback: String,
    /// Optional filter to apply to advertisements; if `None`, all advertisements are reported.
    pub filter: Option<DiscoveryFilter>,
    /// Whether to also receive scan responses.
    pub mode: ScanMode,
}

/// Request to connect to a peripheral.
///
/// The host invokes `on_connected` with a [`ConnectionInfo`] once the link is up
/// and services are discovered, or `on_disconnected` with a [`DisconnectReason`]
/// if the connection fails to establish or later drops.
#[cfg(feature = "alloc")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectRequest {
    /// MAC address of the peripheral to connect to.
    pub address: Address,
    /// Command invoked with a [`ConnectionInfo`] once the link is up.
    pub on_connected: String,
    /// Command invoked with a [`DisconnectReason`] when the connection fails
    /// or later drops.
    pub on_disconnected: String,
}

/// Request to subscribe to notifications on a characteristic.
///
/// The host invokes `callback` with a [`NotificationInfo`] for each notification.
#[cfg(feature = "alloc")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscribeRequest {
    /// The connection handle from [`ConnectionInfo::id`].
    pub connection_id: u32,
    /// The characteristic to subscribe to.
    pub characteristic: Characteristic,
    /// Command invoked with a [`NotificationInfo`] per notification.
    pub callback: String,
}

/// Request to read a characteristic.
///
/// The host invokes `callback` with a [`ReadOutcome`] carrying the value read or
/// the error encountered.
#[cfg(feature = "alloc")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadRequest {
    /// The connection handle from [`ConnectionInfo::id`].
    pub connection_id: u32,
    /// The characteristic to read.
    pub characteristic: Characteristic,
    /// Command invoked with the [`ReadOutcome`].
    pub callback: String,
}

/// Request to write a characteristic.
///
/// When `callback` is `Some`, the write expects a response and the host invokes
/// it with a [`WriteOutcome`]; when `None`, the value is written without a
/// response and no callback is invoked.
#[cfg(feature = "alloc")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteRequest {
    /// The connection handle from [`ConnectionInfo::id`].
    pub connection_id: u32,
    /// The characteristic to write.
    pub characteristic: Characteristic,
    /// The value to write.
    pub data: Vec<u8>,
    /// Command invoked with the [`WriteOutcome`]; `None` writes without
    /// response.
    pub callback: Option<String>,
}

/// Delivered to a connect callback once the link is established.
///
/// Carries the `id` used to address the connection in later operations, together
/// with the discovered GATT services.
#[cfg(feature = "alloc")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionInfo {
    /// Connection handle
    pub id: u32,
    /// Mac address of the connected device
    pub mac_address: Address,
    /// Discovered GATT services, keyed by service UUID.
    pub gatt_services: BTreeMap<Uuid, Service>,
}

/// Delivered to a notification callback for each notification received.
#[cfg(feature = "alloc")]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct NotificationInfo {
    pub characteristic: Characteristic,
    pub data: Vec<u8>,
}

/// Delivered to a read callback with the value read or the error encountered.
#[cfg(feature = "alloc")]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct ReadOutcome {
    pub characteristic: Characteristic,
    pub value: Result<Vec<u8>, ReadError>,
}

/// Delivered to a write callback with the result of a write-with-response.
#[cfg(feature = "alloc")]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct WriteOutcome {
    pub characteristic: Characteristic,
    pub result: Result<(), WriteError>,
}
