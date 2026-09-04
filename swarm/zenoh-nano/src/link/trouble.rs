//! Bluetooth LE GATT link using the Trouble BLE host stack.

use core::future::Future;

use bt_hci::ControllerToHostPacket;
use bt_hci::cmd::{AsyncCmd, SyncCmd};
use bt_hci::controller::{ControllerCmdAsync, ControllerCmdSync};
use bt_hci::data::{AclPacket, IsoPacket, SyncPacket};

use embassy_sync::blocking_mutex::raw::{NoopRawMutex, RawMutex};
use embassy_sync::zerocopy_channel::{Channel, Receiver, Sender};

use embedded_io_async::ErrorType;

use trouble_host;
use trouble_host::prelude::*;

use zenoh_buffers::ZSlice;

use crate::link::{LinkError, LinkReceive, LinkSend};

#[cfg(feature = "trouble-accept")]
pub use accept::*;
#[cfg(feature = "trouble-connect")]
pub use connect::*;

#[cfg(feature = "trouble-accept")]
mod accept;
#[cfg(feature = "trouble-connect")]
mod connect;

/// The resources required to create a GattLink.
pub struct GattLinkResources {
    incoming_buffer: [Payload; 1],
    outgoing_buffer: [Payload; 1],
}

impl GattLinkResources {
    /// Create a new instance.
    #[inline(always)]
    pub const fn new() -> Self {
        Self {
            incoming_buffer: [Payload::new()],
            outgoing_buffer: [Payload::new()],
        }
    }
}

impl Default for GattLinkResources {
    #[inline(always)]
    fn default() -> Self {
        Self::new()
    }
}

/// A Bluetooth LE GATT link using Trouble host stack.
pub struct GattLink<'a, M = NoopRawMutex>
where
    M: RawMutex,
{
    /// Incoming channel.
    ///
    /// The data on this channel is sent by the remote device.
    incoming: Channel<'a, M, Payload>,
    /// Outgoing channel.
    ///
    /// The data on this channel is sent to the remote device.
    outgoing: Channel<'a, M, Payload>,
}

impl<'a, M> GattLink<'a, M>
where
    M: RawMutex,
{
    /// Create a new instance.
    ///
    /// # Arguments
    /// - `resources`: The resources required to create the GattLink.
    pub fn new(resources: &'a mut GattLinkResources) -> Self {
        Self {
            incoming: Channel::new(&mut resources.incoming_buffer),
            outgoing: Channel::new(&mut resources.outgoing_buffer),
        }
    }
}

/// The send half of a GATT link.
///
/// Implements the `LinkSend` trait.
pub struct GattLinkSend<'a, M = NoopRawMutex>
where
    M: RawMutex,
{
    /// Negotiated ATT MTU - 3 bytes for GATT header
    mtu: u16,
    /// Outgoing channel sender.
    sender: Sender<'a, M, Payload>,
}

impl<M> LinkSend for GattLinkSend<'_, M>
where
    M: RawMutex,
{
    async fn send(&mut self, payload: ZSlice) -> Result<(), LinkError> {
        trace!("GattLinkSend: sending {} bytes", payload.len());

        self.sender.send().await.set(payload.as_slice());

        self.sender.send_done();

        trace!("GattLinkSend: sent {} bytes", payload.len());

        Ok(())
    }

    fn mtu(&self) -> u16 {
        self.mtu
    }
}

/// The receive half of a GATT link.
pub struct GattLinkReceive<'a, M = NoopRawMutex>
where
    M: RawMutex,
{
    /// Negotiated ATT MTU - 3 bytes for GATT header
    mtu: u16,
    /// Incoming channel receiver.
    receiver: Receiver<'a, M, Payload>,
}

impl<M> LinkReceive for GattLinkReceive<'_, M>
where
    M: RawMutex,
{
    async fn receive(&mut self) -> Result<ZSlice, LinkError> {
        trace!("GattLinkReceive: waiting to receive data");

        let data: alloc::vec::Vec<u8> = self.receiver.receive().await.get().into();

        self.receiver.receive_done();

        trace!("GattLinkReceive: received {} bytes", data.len());

        Ok(data.into())
    }

    fn mtu(&self) -> u16 {
        self.mtu
    }
}

/// Payload structure for GATT link communication.
#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
struct Payload {
    /// Data payload.
    data: [u8; DefaultPacketPool::MTU],
    len: usize,
}

impl Payload {
    /// Create a new instance.
    #[inline(always)]
    const fn new() -> Self {
        Self {
            data: [0; DefaultPacketPool::MTU],
            len: 0,
        }
    }

    /// Get the data payload.
    fn get(&self) -> &[u8] {
        &self.data[..self.len]
    }

    /// Set the data payload.
    fn set(&mut self, data: &[u8]) {
        self.data[..data.len()].copy_from_slice(data);
        self.len = data.len();
    }
}

/// The Zenoh GATT Service UUID.
pub const SERVICE_UUID: u128 = 0x24A9597F_1060_41BB_AB31_B638662BDCCC;

/// The Zenoh GATT RX Characteristic UUID
const RX_CHAR_UUID: u128 = 0x7E54E1BC_82BF_4B0E_9B3A_3C187934BD89;

/// The Zenoh GATT TX Characteristic UUID
const TX_CHAR_UUID: u128 = 0xF47EA3E5_4D04_4EEE_9ACA_E397C4408952;

/// Custom AD type carrying the Zenoh role byte in legacy BLE advertisements.
///
/// We use `0x10`, because `trouble` does not decode that value as the standard `Flags` AD type.
const AD_TYPE_ZENOH_ROLE: u8 = 0x10;

/// A newtype allowing to use a bt_hci `&Controller` as a `Controller`
/// A workaround for:
/// <https://github.com/embassy-rs/bt-hci/issues/32>
pub struct ControllerRef<'a, C>(&'a C);

impl<'a, C> ControllerRef<'a, C> {
    /// Create a new instance.
    pub const fn new(controller: &'a C) -> Self {
        Self(controller)
    }
}

impl<C> ErrorType for ControllerRef<'_, C>
where
    C: ErrorType,
{
    type Error = C::Error;
}

impl<C> bt_hci::controller::Controller for ControllerRef<'_, C>
where
    C: bt_hci::controller::Controller,
{
    fn write_acl_data(&self, packet: &AclPacket) -> impl Future<Output = Result<(), Self::Error>> {
        self.0.write_acl_data(packet)
    }

    fn write_sync_data(
        &self,
        packet: &SyncPacket,
    ) -> impl Future<Output = Result<(), Self::Error>> {
        self.0.write_sync_data(packet)
    }

    fn write_iso_data(&self, packet: &IsoPacket) -> impl Future<Output = Result<(), Self::Error>> {
        self.0.write_iso_data(packet)
    }

    fn read<'a>(
        &self,
        buf: &'a mut [u8],
    ) -> impl Future<Output = Result<ControllerToHostPacket<'a>, Self::Error>> {
        self.0.read(buf)
    }
}

impl<C> bt_hci::controller::blocking::Controller for ControllerRef<'_, C>
where
    C: bt_hci::controller::blocking::Controller,
{
    fn write_acl_data(&self, packet: &AclPacket) -> Result<(), Self::Error> {
        self.0.write_acl_data(packet)
    }

    fn write_sync_data(&self, packet: &SyncPacket) -> Result<(), Self::Error> {
        self.0.write_sync_data(packet)
    }

    fn write_iso_data(&self, packet: &IsoPacket) -> Result<(), Self::Error> {
        self.0.write_iso_data(packet)
    }

    fn try_write_acl_data(
        &self,
        packet: &AclPacket,
    ) -> Result<(), bt_hci::controller::blocking::TryError<Self::Error>> {
        self.0.try_write_acl_data(packet)
    }

    fn try_write_sync_data(
        &self,
        packet: &SyncPacket,
    ) -> Result<(), bt_hci::controller::blocking::TryError<Self::Error>> {
        self.0.try_write_sync_data(packet)
    }

    fn try_write_iso_data(
        &self,
        packet: &IsoPacket,
    ) -> Result<(), bt_hci::controller::blocking::TryError<Self::Error>> {
        self.0.try_write_iso_data(packet)
    }

    fn read<'a>(&self, buf: &'a mut [u8]) -> Result<ControllerToHostPacket<'a>, Self::Error> {
        self.0.read(buf)
    }

    fn try_read<'a>(
        &self,
        buf: &'a mut [u8],
    ) -> Result<ControllerToHostPacket<'a>, bt_hci::controller::blocking::TryError<Self::Error>>
    {
        self.0.try_read(buf)
    }
}

impl<C, Q> ControllerCmdSync<Q> for ControllerRef<'_, C>
where
    C: ControllerCmdSync<Q>,
    Q: SyncCmd + ?Sized,
{
    fn exec(
        &self,
        cmd: &Q,
    ) -> impl Future<Output = Result<Q::Return, bt_hci::cmd::Error<Self::Error>>> {
        self.0.exec(cmd)
    }
}

impl<C, Q> ControllerCmdAsync<Q> for ControllerRef<'_, C>
where
    C: ControllerCmdAsync<Q>,
    Q: AsyncCmd + ?Sized,
{
    fn exec(&self, cmd: &Q) -> impl Future<Output = Result<(), bt_hci::cmd::Error<Self::Error>>> {
        self.0.exec(cmd)
    }
}
