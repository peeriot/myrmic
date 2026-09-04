//! BLE Host Functions
//!
//! Callback-oriented BLE API for cells. Instead of blocking, an operation registers a [`Callback`]
//! naming one of the cell's own `#[cmd]` handlers; the host invokes that command later, when the
//! result is ready:
//!
//! - [`scan`] delivers a [`DiscoveredDevice`] per matching advertisement.
//! - [`connect`] delivers a [`Connection`] on success, or a [`Disconnect`] when the link fails to
//!   establish or later drops.
//! - [`Connection::subscribe`] delivers a [`Notification`] per notification.
//! - [`Connection::read`] / [`Connection::write`] deliver a
//!   [`myrmic_common::types::ble::ReadOutcome`] /
//!   [`myrmic_common::types::ble::WriteOutcome`].
//!
//! The returned handles ([`ScanHandle`], [`Connection`], [`Subscription`]) own a host-side
//! resource. Dropping a handle does NOT release that resource - it keeps running on the host - so a
//! cell that needs to stop, read, or tear one down in a later invocation must persist it in the
//! data layer.

use core::ffi::c_int;

use myrmic_common::types::ble::{
    ConnectRequest, ConnectionInfo, DisconnectReason, NotificationInfo, ReadOutcome, ReadRequest,
    ScanRequest, SubscribeRequest, WriteOutcome, WriteRequest,
};
use myrmic_common::types::error::{EACCES, EPERM};
use serde::{Deserialize, Serialize};

use crate::error::ErrorCode;
use crate::{
    ApiError, ApiResult, Bytes, Callback, Codec, Command, Decoder, Postcard, Result, String,
};

pub use myrmic_common::types::ble::{
    Address, Advertisement, Characteristic, DiscoveredDevice, DiscoveryFilter, ManufacturerData,
    NotifyError, ReadError, ScanMode, Service, ServiceData, Uuid, WriteError,
};
pub use myrmic_common::{mac_addr_pub, mac_addr_rand, uuid128};

mod c_functions {
    use core::ffi::c_int;

    #[link(wasm_import_module = "ble")]
    unsafe extern "C" {
        /// Starts a filtered scan from a serialized [`super::ScanRequest`].
        ///
        /// # Returns
        /// - [`myrmic_common::types::error::SUCCESS`] on success
        /// - negative error code on failure
        pub(super) fn ble_scan(request: *const u8, request_len: c_int) -> c_int;

        /// Stops the cell's active scan (there is at most one per cell).
        ///
        /// # Returns
        /// - [`myrmic_common::types::error::SUCCESS`] on success
        /// - negative error code on failure
        pub(super) fn ble_stop_scan() -> c_int;

        /// Initiates a connection from a serialized [`super::ConnectRequest`].
        /// The outcome is delivered asynchronously to the request's callbacks.
        ///
        /// # Returns
        /// - [`myrmic_common::types::error::SUCCESS`] once the attempt is queued
        /// - negative error code on failure
        pub(super) fn ble_connect(request: *const u8, request_len: c_int) -> c_int;

        /// Disconnects the connection identified by `id`.
        ///
        /// # Returns
        /// - [`myrmic_common::types::error::SUCCESS`] on success
        /// - negative error code on failure
        pub(super) fn ble_disconnect(id: c_int) -> c_int;

        /// Subscribes to notifications from a serialized [`super::SubscribeRequest`].
        ///
        /// # Returns
        /// - subscription id (>= 0) on success
        /// - [`myrmic_common::types::error::EPERM`] if the characteristic is not notifiable
        /// - [`myrmic_common::types::error::EACCES`] if the characteristic requires security
        /// - other negative error code on failure
        pub(super) fn ble_subscribe(request: *const u8, request_len: c_int) -> c_int;

        /// Unsubscribes the subscription identified by `id`.
        ///
        /// # Returns
        /// - [`myrmic_common::types::error::SUCCESS`] on success
        /// - negative error code on failure
        pub(super) fn ble_unsubscribe(id: c_int) -> c_int;

        /// Reads a characteristic from a serialized [`super::ReadRequest`]. The
        /// value is delivered asynchronously to the request's callback.
        ///
        /// # Returns
        /// - [`myrmic_common::types::error::SUCCESS`] once the read is queued
        /// - negative error code on failure
        pub(super) fn ble_read(request: *const u8, request_len: c_int) -> c_int;

        /// Writes a characteristic from a serialized [`super::WriteRequest`]. For
        /// a write-with-response the result is delivered to the request's callback.
        ///
        /// # Returns
        /// - [`myrmic_common::types::error::SUCCESS`] once the write is queued
        /// - negative error code on failure
        pub(super) fn ble_write(request: *const u8, request_len: c_int) -> c_int;

        /// Enables pairing with the given static passkey (encoded as a `u32`).
        ///
        /// # Returns
        /// - [`myrmic_common::types::error::SUCCESS`] on success
        /// - negative error code on failure
        pub(super) fn ble_set_pair_passkey(passkey: c_int) -> c_int;
    }
}

/// Handle to an active scan.
///
/// Dropping the handle does NOT stop the scan - it keeps running on the host.
/// Store it in cell state to retain the ability to [`stop`](ScanHandle::stop) it.
///
/// A cell has at most one active scan, so the handle carries no id - it is simply
/// the cell's claim on its scan, and [`stop`](ScanHandle::stop) stops that scan.
#[must_use = "dropping the handle loses the ability to stop the scan - store it in cell state"]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanHandle;

/// Handle to an established connection.
///
/// Delivered to the `on_connected` callback and used to interact with the peripheral's GATT
/// services. Dropping the handle does NOT disconnect - the link stays up on the host. Store it in
/// cell state to retain the ability to [`disconnect`](Connection::disconnect) or issue further
/// operations.
#[must_use = "dropping the connection loses the ability to disconnect it - store it in cell state"]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    info: ConnectionInfo,
}

/// Handle to an active notification subscription.
///
/// Dropping the handle does NOT unsubscribe - notifications keep arriving on the
/// host. Store it in cell state to retain the ability to
/// [`unsubscribe`](Subscription::unsubscribe).
#[must_use = "dropping the handle loses the ability to unsubscribe - store it in cell state"]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    id: u32,
}

/// A single notification, delivered to a subscription's callback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    info: NotificationInfo,
}

/// A connection ending, delivered to a connect's `on_disconnected` callback.
///
/// Covers both a failure to establish the link and a later drop; the
/// [`reason`](Disconnect::reason) distinguishes them.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Disconnect {
    reason: DisconnectReason,
}

/// Starts scanning for peripherals optionally matching `filter`.
///
/// The host applies the filter inside its scanning loop and invokes `callback`
/// with a [`DiscoveredDevice`] for each matching advertisement, so the cell
/// never sees unrelated devices. Scanning continues until the returned
/// [`ScanHandle`] is [`stop`](ScanHandle::stop)ped.
///
/// `mode` selects whether scan responses are also requested; see [`ScanMode`]
/// for when [`ScanMode::Active`] is needed.
pub fn scan(
    callback: Callback<DiscoveredDevice>,
    filter: Option<DiscoveryFilter>,
    mode: ScanMode,
) -> ApiResult<ScanHandle> {
    let request = ScanRequest {
        callback: command_name(callback),
        filter,
        mode,
    };
    send_request(&request, c_functions::ble_scan)?;

    Ok(ScanHandle)
}

/// Begins building a connection to the peripheral at `address`.
///
/// Both `on_connected` and `on_disconnected` must be supplied before
/// [`initiate`](ConnectBuilder::initiate).
///
/// # Note
///
/// Some backends can allow a cold connect (scanless), where you can skip the scan step and connect
/// directly to a peripheral. Others require a warm connect, where you must first scan and discover
/// the peripheral before connecting. If you attempt a cold connect on a backend that requires a
/// warm connect, the connection will fail with [`DisconnectReason::ConnectionFailed`].
/// It's generally preferable to scan first, for consistency.
#[must_use]
pub fn connect(address: Address) -> ConnectBuilder {
    ConnectBuilder {
        address,
        on_connected: None,
        on_disconnected: None,
    }
}

/// Enables pairing with the given static passkey.
pub fn set_pair_passkey(passkey: u32) -> ApiResult<()> {
    // SAFETY: passes a scalar, no linear-memory access.
    unsafe { c_functions::ble_set_pair_passkey(passkey.cast_signed()) }.to_result()
}

/// Builder for [`connect`].
pub struct ConnectBuilder {
    address: Address,
    on_connected: Option<String>,
    on_disconnected: Option<String>,
}

impl ConnectBuilder {
    /// Sets the command invoked with the [`Connection`] once the link is up.
    #[must_use]
    pub fn on_connected(mut self, callback: Callback<Connection>) -> Self {
        self.on_connected = Some(command_name(callback));
        self
    }

    /// Sets the command invoked with a [`Disconnect`] if the link fails to
    /// establish or later drops.
    #[must_use]
    pub fn on_disconnected(mut self, callback: Callback<Disconnect>) -> Self {
        self.on_disconnected = Some(command_name(callback));
        self
    }

    /// Initiates the connection. The outcome arrives on one of the callbacks.
    pub fn initiate(self) -> ApiResult<()> {
        let request = ConnectRequest {
            address: self.address,
            on_connected: self.on_connected.ok_or(ApiError::Usage)?,
            on_disconnected: self.on_disconnected.ok_or(ApiError::Usage)?,
        };
        send_request(&request, c_functions::ble_connect)?;

        Ok(())
    }
}

impl ScanHandle {
    /// Stops the scan, consuming the handle.
    pub fn stop(self) -> ApiResult<()> {
        // SAFETY: no linear-memory access; stops the cell's single active scan.
        unsafe { c_functions::ble_stop_scan() }.to_result()
    }
}

impl Connection {
    /// The peripheral's address.
    pub fn address(&self) -> Address {
        self.info.mac_address
    }

    /// Finds a characteristic by its service UUID and characteristic UUID among
    /// the services discovered on connect.
    pub fn characteristic(&self, service: Uuid, characteristic: Uuid) -> Option<Characteristic> {
        self.info
            .gatt_services
            .get(&service)?
            .characteristics
            .get(&characteristic)
            .copied()
    }

    /// Subscribes to notifications on `characteristic`.
    ///
    /// The registration result is returned synchronously; on success each
    /// notification is delivered to `callback` as a [`Notification`].
    pub fn subscribe(
        &self,
        characteristic: Characteristic,
        callback: Callback<Notification>,
    ) -> ApiResult<core::result::Result<Subscription, NotifyError>> {
        let request = SubscribeRequest {
            connection_id: self.info.id,
            characteristic,
            callback: command_name(callback),
        };
        let bytes = serialize(&request)?;
        // SAFETY: the host reads `bytes.len()` bytes from the provided pointer.
        let ret = unsafe { c_functions::ble_subscribe(bytes.as_ptr(), bytes.len() as c_int) };

        match ret {
            id if id >= 0 => Ok(Ok(Subscription { id: id as u32 })),
            EPERM => Ok(Err(NotifyError::NotNotifiable)),
            EACCES => Ok(Err(NotifyError::RequiresSecurity)),
            other => Err(other.into()),
        }
    }

    /// Reads `characteristic`. The value is delivered to `callback` as a [`ReadOutcome`].
    pub fn read(
        &self,
        characteristic: Characteristic,
        callback: Callback<ReadOutcome>,
    ) -> ApiResult<()> {
        let request = ReadRequest {
            connection_id: self.info.id,
            characteristic,
            callback: command_name(callback),
        };
        send_request(&request, c_functions::ble_read)?;

        Ok(())
    }

    /// Writes `data` to `characteristic` and expects a response. The result is delivered to
    /// `callback` as a [`WriteOutcome`].
    pub fn write(
        &self,
        characteristic: Characteristic,
        data: &[u8],
        callback: Callback<WriteOutcome>,
    ) -> ApiResult<()> {
        let request = WriteRequest {
            connection_id: self.info.id,
            characteristic,
            data: data.to_vec(),
            callback: Some(command_name(callback)),
        };
        send_request(&request, c_functions::ble_write)?;

        Ok(())
    }

    /// Writes `data` to `characteristic` without waiting for a response.
    pub fn write_no_response(&self, characteristic: Characteristic, data: &[u8]) -> ApiResult<()> {
        let request = WriteRequest {
            connection_id: self.info.id,
            characteristic,
            data: data.to_vec(),
            callback: None,
        };
        send_request(&request, c_functions::ble_write)?;

        Ok(())
    }

    /// Disconnects, consuming the handle.
    pub fn disconnect(self) -> ApiResult<()> {
        // SAFETY: the host owns the connection identified by `id`.
        unsafe { c_functions::ble_disconnect(self.info.id as c_int) }.to_result()
    }
}

impl Subscription {
    /// Unsubscribes, consuming the handle.
    pub fn unsubscribe(self) -> ApiResult<()> {
        // SAFETY: the host owns the subscription identified by `id`.
        unsafe { c_functions::ble_unsubscribe(self.id as c_int) }.to_result()
    }
}

impl Notification {
    /// The characteristic that produced this notification.
    pub fn characteristic(&self) -> Characteristic {
        self.info.characteristic
    }

    /// The notification payload.
    pub fn data(&self) -> &[u8] {
        &self.info.data
    }
}

impl Disconnect {
    /// Why the connection ended.
    pub fn reason(&self) -> DisconnectReason {
        self.reason
    }
}

impl core::fmt::Display for Disconnect {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.reason.fmt(f)
    }
}

impl Decoder for DiscoveredDevice {
    fn from_bytes(bytes: Bytes) -> Result<Self> {
        <Postcard as Codec>::decode(&bytes)
    }
}

impl Decoder for Connection {
    fn from_bytes(bytes: Bytes) -> Result<Self> {
        Ok(Connection {
            info: <Postcard as Codec>::decode(&bytes)?,
        })
    }
}

impl Decoder for Notification {
    fn from_bytes(bytes: Bytes) -> Result<Self> {
        Ok(Notification {
            info: <Postcard as Codec>::decode(&bytes)?,
        })
    }
}

impl Decoder for Disconnect {
    fn from_bytes(bytes: Bytes) -> Result<Self> {
        Ok(Disconnect {
            reason: <Postcard as Codec>::decode(&bytes)?,
        })
    }
}

impl Decoder for ReadOutcome {
    fn from_bytes(bytes: Bytes) -> Result<Self> {
        <Postcard as Codec>::decode(&bytes)
    }
}

impl Decoder for WriteOutcome {
    fn from_bytes(bytes: Bytes) -> Result<Self> {
        <Postcard as Codec>::decode(&bytes)
    }
}

/// Postcard-serializes a request, mapping failure to [`ApiError::Serde`].
fn serialize(request: &impl Serialize) -> ApiResult<Bytes> {
    postcard::to_allocvec(request).map_err(|_| ApiError::Serde("unable to serialise ble request"))
}

/// Serializes `request`, calls a request-taking import, and maps a negative
/// return to an error and a non-negative return to itself (an id or success).
fn send_request(
    request: &impl Serialize,
    func: unsafe extern "C" fn(*const u8, c_int) -> c_int,
) -> ApiResult<c_int> {
    let bytes = serialize(request)?;
    // SAFETY: the host reads `bytes.len()` bytes from the provided pointer.
    let ret = unsafe { func(bytes.as_ptr(), bytes.len() as c_int) };

    if ret >= 0 { Ok(ret) } else { Err(ret.into()) }
}

/// Extracts the wire command name a callback targets.
fn command_name<T>(callback: Callback<T>) -> String {
    let command: Command = callback.into();

    command.as_ref().into()
}
