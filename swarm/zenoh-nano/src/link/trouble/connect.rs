//! Connect to a GATT Zenoh service on a remote device.

use core::pin::pin;

use bt_hci::FromHciBytesError;
use bt_hci::cmd::le::{
    LeAddDeviceToFilterAcceptList, LeClearFilterAcceptList, LeSetScanEnable, LeSetScanParams,
};
use bt_hci::controller::ControllerCmdSync;
use bt_hci::param::{LeAdvReport, LeExtAdvReport};

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::{NoopRawMutex, RawMutex};
use embassy_sync::zerocopy_channel::{Receiver, Sender};

use trouble_host::prelude::*;
use trouble_host::{self, BleHostError, Controller};
use zenoh_protocol::core::WhatAmI;

use crate::fmt::Bytes;

use super::{
    AD_TYPE_ZENOH_ROLE, GattLink, GattLinkReceive, GattLinkSend, Payload, RX_CHAR_UUID,
    SERVICE_UUID, TX_CHAR_UUID,
};

impl<M> GattLink<'_, M>
where
    M: RawMutex,
{
    /// Connect to a GATT Zenoh service on a remote device.
    ///
    /// # Arguments
    /// - `stack`: The stack to use for connecting.
    /// - `addr`: The address of the remote device.
    ///
    /// # Returns
    /// - A tuple containing the GattLinkRunner, GattLinkSend, and GattLinkReceive.
    pub async fn connect<'s, C>(
        &mut self,
        stack: &'s Stack<'s, C, DefaultPacketPool>,
        addr: Address,
    ) -> Result<
        (
            GattLinkConnectRunner<'s, '_, C, M>,
            GattLinkReceive<'_, M>,
            GattLinkSend<'_, M>,
        ),
        BleHostError<C::Error>,
    >
    where
        C: Controller,
    {
        let mut central = stack.central();

        let conn = central
            .connect(&ConnectConfig {
                scan_config: ScanConfig {
                    filter_accept_list: &[addr],
                    ..Default::default()
                },
                connect_params: RequestedConnParams::default(),
            })
            .await?;

        debug!(
            "Connected to {:?}, negotiated ATT MTU {}",
            addr.addr,
            conn.att_mtu()
        );

        // TODO: This only comes later, unfortunately, the initially reported MTU is the minim one - 23
        // We might have to delay the MTU usage until after the connection is fully established
        // and push the connection into GattLinkSend / GattLinkReceive
        let mtu = 244; //conn.att_mtu().saturating_sub(3) as u16;

        let (incoming_send, incoming_recv) = self.incoming.split();
        let (outgoing_send, outgoing_recv) = self.outgoing.split();

        Ok((
            GattLinkConnectRunner {
                stack,
                conn,
                incoming: incoming_send,
                outgoing: outgoing_recv,
            },
            GattLinkReceive {
                mtu,
                receiver: incoming_recv,
            },
            GattLinkSend {
                mtu,
                sender: outgoing_send,
            },
        ))
    }
}

/// A GATT link runner for a connection initiated by us.
pub struct GattLinkConnectRunner<'s, 'a, C, M = NoopRawMutex>
where
    M: RawMutex,
{
    /// BLE stack.
    stack: &'s Stack<'s, C, DefaultPacketPool>,
    /// Active connection.
    conn: Connection<'s, DefaultPacketPool>,
    /// Incoming channel.
    /// The data on this channel is sent by the remote device via notifications or indications to characteristic `TX_CHAR_UUID`.
    incoming: Sender<'a, M, Payload>,
    /// Outgoing channel.
    /// The data on this channel is sent to the remote device via writes to characteristic `RX_CHAR_UUID`.
    outgoing: Receiver<'a, M, Payload>,
}

impl<C, M> GattLinkConnectRunner<'_, '_, C, M>
where
    C: Controller,
    M: RawMutex,
{
    /// Run the GATT link runner.
    pub async fn run(mut self) -> Result<(), BleHostError<C::Error>> {
        let client = GattClient::<_, DefaultPacketPool, 10>::new(self.stack, &self.conn).await?;

        let mut task = pin!(client.task());
        let mut inout = pin!(Self::run_inout(
            &client,
            &mut self.incoming,
            &mut self.outgoing
        ));

        match select(&mut task, &mut inout).await {
            Either::First(res) => res,
            Either::Second(res) => res,
        }
    }

    /// Run the incoming and outgoing handlers.
    ///
    /// # Arguments
    /// - `client`: The GATT client.
    /// - `incoming`: The sender side of the incoming channel.
    /// - `outgoing`: The receiver side of the outgoing channel.
    async fn run_inout(
        client: &GattClient<'_, C, DefaultPacketPool, 10>,
        incoming: &mut Sender<'_, M, Payload>,
        outgoing: &mut Receiver<'_, M, Payload>,
    ) -> Result<(), BleHostError<C::Error>> {
        embassy_time::Timer::after(embassy_time::Duration::from_millis(2000)).await;

        debug!("GATT: Discovering services...");

        let services = client
            .services_by_uuid(&Uuid::Uuid128(SERVICE_UUID.to_le_bytes()))
            .await?;

        let service = services.iter().next().unwrap(); // TODO

        debug!("GATT: Found service {:?}", service);

        let tx_char = client
            .characteristic_by_uuid::<u8>(service, &Uuid::Uuid128(RX_CHAR_UUID.to_le_bytes()))
            .await?;

        let rx_char = client
            .characteristic_by_uuid::<u8>(service, &Uuid::Uuid128(TX_CHAR_UUID.to_le_bytes()))
            .await?;

        let mut listener = client.subscribe(&rx_char, true).await?;

        debug!(
            "GATT: Subscribed to notifications on characteristic {:?}",
            rx_char
        );

        let mut incoming = pin!(Self::handle_incoming(client, &mut listener, incoming));
        let mut outgoing = pin!(Self::handle_outgoing(client, tx_char, outgoing));

        match select(&mut incoming, &mut outgoing).await {
            Either::First(res) => res,
            Either::Second(res) => res,
        }
    }

    /// Handle incoming indications.
    ///
    /// # Arguments
    /// - `client`: The GATT client.
    /// - `listener`: The indications' listener.
    /// - `incoming`: The sender side of the incoming channel.
    async fn handle_incoming<const N: usize, const MTU: usize>(
        client: &GattClient<'_, C, DefaultPacketPool, N>,
        listener: &mut NotificationListener<'_, MTU>,
        incoming: &mut Sender<'_, M, Payload>,
    ) -> Result<(), BleHostError<C::Error>> {
        debug!("GATT: Starting incoming handler");

        loop {
            let payload = incoming.send().await;

            trace!("GATT: Waiting for notification...");

            let ind = listener.next().await;

            trace!("GATT: Received {} bytes", ind.as_ref().len());

            payload.set(ind.as_ref());

            trace!(
                "GATT: Indicate {} len {}",
                Bytes(payload.get()),
                payload.get().len()
            );

            incoming.send_done();

            trace!("GATT: Indication processed");

            if let Err(e) = client.confirm_indication().await {
                error!("GATT: Failed to confirm indication: {:?}", e);
            }
        }
    }

    /// Handle outgoing writes.
    ///
    /// # Arguments
    /// - `client`: The GATT client.
    /// - `tx_char`: The TX characteristic where to write the link data.
    /// - `outgoing`: The receiver side of the outgoing channel.
    async fn handle_outgoing<const N: usize>(
        client: &GattClient<'_, C, DefaultPacketPool, N>,
        tx_char: Characteristic<u8>,
        outgoing: &mut Receiver<'_, M, Payload>,
    ) -> Result<(), BleHostError<C::Error>>
    where
        C: Controller,
    {
        debug!("GATT: Starting outgoing handler");

        loop {
            let payload = outgoing.receive().await;

            trace!("GATT: Writing {} bytes", payload.get().len());

            client.write_characteristic(&tx_char, payload.get()).await?;
            //client.write_characteristic_without_response(&tx_char, &payload.data).await?;

            trace!(
                "GATT: Wrote {} len {}",
                Bytes(payload.get()),
                payload.get().len()
            );

            outgoing.receive_done();
        }
    }
}

/// Perform a scan for devices.
///
/// The future of this method never completes, it just keeps scanning.
/// To cancel the scan, the returned future must be dropped.
///
/// # Arguments
/// - `stack`: The stack to use for scanning.
pub async fn scan<'s, C, P>(stack: &'s Stack<'s, C, P>) -> Result<(), BleHostError<C::Error>>
where
    C: Controller
        + ControllerCmdSync<LeSetScanParams>
        + ControllerCmdSync<LeSetScanEnable>
        + ControllerCmdSync<LeClearFilterAcceptList>
        + ControllerCmdSync<LeAddDeviceToFilterAcceptList>,
    P: PacketPool,
{
    let central = stack.central();

    let mut scanner = Scanner::new(central);

    let _session = scanner.scan(&ScanConfig::default()).await?;

    core::future::pending::<()>().await;

    Ok(())
}

/// A small abstraction over the two different advertisement report types supported by `trouble-host` -
/// `LeAdvReport` and `LeExtAdvReport`.
pub trait TroubleAd {
    /// Get the address of the advertisement.
    fn addr(&self) -> Address;

    /// Get the data of the advertisement.
    fn data(&self) -> &[u8];

    /// Extract the advertised Zenoh role, if present.
    fn zenoh_role(&self) -> Option<WhatAmI> {
        for ad_item in AdStructure::decode(self.data()) {
            let ad = ad_item.ok()?;

            if let AdStructure::Unknown {
                ty: AD_TYPE_ZENOH_ROLE,
                data,
            } = ad
            {
                if data.len() != 1 {
                    continue;
                }

                return WhatAmI::try_from(data[0]).ok();
            }
        }

        None
    }

    /// Check if an LE advertisement report contains the Zenoh service UUID.
    ///
    /// # Arguments
    /// - `ad`: The LE advertisement report to check.
    ///
    /// # Returns
    /// - `true` if the advertisement report contains the Zenoh service UUID, `false` otherwise.
    fn is_zenoh_ad(&self) -> bool {
        for ad_item in AdStructure::decode(self.data()) {
            let ad = ad_item.unwrap(); // TODO

            if let AdStructure::CompleteServiceUuids128(uuids)
            | AdStructure::IncompleteServiceUuids128(uuids) = ad
            {
                for uuid in uuids {
                    if uuid == &SERVICE_UUID.to_le_bytes() {
                        return true;
                    }
                }
            }
        }

        false
    }
}

impl<T> TroubleAd for &T
where
    T: TroubleAd,
{
    fn addr(&self) -> Address {
        (*self).addr()
    }

    fn data(&self) -> &[u8] {
        (*self).data()
    }

    fn zenoh_role(&self) -> Option<WhatAmI> {
        (*self).zenoh_role()
    }

    fn is_zenoh_ad(&self) -> bool {
        (*self).is_zenoh_ad()
    }
}

impl TroubleAd for LeAdvReport<'_> {
    fn addr(&self) -> Address {
        Address {
            kind: self.addr_kind,
            addr: self.addr,
        }
    }

    fn data(&self) -> &[u8] {
        self.data
    }
}

impl TroubleAd for LeExtAdvReport<'_> {
    fn addr(&self) -> Address {
        Address {
            kind: self.addr_kind,
            addr: self.addr,
        }
    }

    fn data(&self) -> &[u8] {
        self.data
    }
}

/// Extract BLE addresses of Zenoh services from a list of LE advertisement reports.
///
/// # Arguments
/// - `ads`: An iterator over LE advertisement reports.
///
/// # Returns
/// - An iterator over BLE addresses of Zenoh services.
pub fn zenoh_addrs<'a, I, A>(ads: I) -> impl Iterator<Item = Address> + 'a
where
    I: Iterator<Item = Result<A, FromHciBytesError>> + 'a,
    A: TroubleAd + 'a,
{
    ads.filter_map(|ad| {
        let ad = ad.unwrap(); // TODO

        if ad.is_zenoh_ad() {
            Some(ad.addr())
        } else {
            None
        }
    })
}
