//! Accept incoming GATT connections for Zenoh over GATT.

use core::pin::pin;

use embassy_futures::select::{Either, select};

use embassy_sync::blocking_mutex::raw::{NoopRawMutex, RawMutex};
use embassy_sync::signal::Signal;
use embassy_sync::zerocopy_channel::{Receiver, Sender};

use embassy_time::Duration;

use trouble_host::att::{AttCfm, AttClient, AttCmd, AttReq, AttRsp, AttUns};
use trouble_host::prelude::*;
use trouble_host::{self, BleHostError, Controller};
use zenoh_protocol::core::WhatAmI;

use crate::fmt::Bytes;
use crate::link::trouble::{
    AD_TYPE_ZENOH_ROLE, GattLink, GattLinkReceive, GattLinkSend, Payload, RX_CHAR_UUID,
    SERVICE_UUID, TX_CHAR_UUID,
};

impl<M> GattLink<'_, M>
where
    M: RawMutex,
{
    /// Advertise the GATT Zenoh service and accept an incoming connection.
    ///
    /// # Arguments
    /// - `stack`: The stack to use for advertising and accepting connections.
    ///
    /// # Returns
    /// - A tuple containing the GattLinkRunner, GattLinkSend, and GattLinkReceive.
    pub async fn accept<'s, C>(
        &mut self,
        stack: &'s Stack<'s, C, DefaultPacketPool>,
    ) -> Result<
        (
            GattLinkAcceptRunner<'s, '_, M>,
            GattLinkReceive<'_, M>,
            GattLinkSend<'_, M>,
        ),
        BleHostError<C::Error>,
    >
    where
        C: Controller + 's,
    {
        let mut peripheral = stack.peripheral();

        let conn = Self::advertise(&mut peripheral, "BB").await?;

        // TODO: This only comes later, unfortunately, the initially reported MTU is the minim one - 23
        // We might have to delay the MTU usage until after the connection is fully established
        // and push the connection into GattLinkSend / GattLinkReceive
        let mtu = 244; //conn.att_mtu().saturating_sub(3) as u16;

        let (incoming_send, incoming_recv) = self.incoming.split();
        let (outgoing_send, outgoing_recv) = self.outgoing.split();

        Ok((
            GattLinkAcceptRunner {
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

    /// Advertise the GATT Zenoh service and accept an incoming connection.
    ///
    /// # Arguments
    /// - `peripheral`: The peripheral to use for advertising and accepting connections.
    /// - `service_name`: The name of the service to advertise.
    ///
    /// # Returns
    /// - A Connection object representing the established connection.
    async fn advertise<'s, C>(
        peripheral: &mut Peripheral<'s, C, DefaultPacketPool>,
        service_name: &str,
    ) -> Result<Connection<'s, DefaultPacketPool>, BleHostError<C::Error>>
    where
        C: Controller,
    {
        debug!("GATT: Advertising Zenoh service");

        let service_uuids = &[SERVICE_UUID.to_le_bytes()];

        let adv_data = [
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            AdStructure::CompleteServiceUuids128(service_uuids),
            AdStructure::Unknown {
                ty: AD_TYPE_ZENOH_ROLE,
                data: &[WhatAmI::Client as u8],
            },
            AdStructure::CompleteLocalName(service_name.as_bytes()),
        ];

        let mut adv_enc_data = [0; 31];
        let len = AdStructure::encode_slice(&adv_data, &mut adv_enc_data)?;

        let advertiser = peripheral
            .advertise(
                // We don't care about speed of visibility, so set min-max intervals to be quite large
                // so that we have more radio time for actual existing connections.
                &AdvertisementParameters {
                    interval_min: Duration::from_millis(1500),
                    interval_max: Duration::from_millis(2000),
                    ..Default::default()
                },
                Advertisement::ConnectableScannableUndirected {
                    adv_data: &adv_enc_data[..len],
                    scan_data: &[],
                },
            )
            .await
            .unwrap();

        info!("GATT: Advertising");

        let conn = advertiser.accept().await.unwrap();

        info!("GATT: Connection established, stop advertising");

        Ok(conn)
    }
}

/// A GATT link runner for a connection accepted by us.
pub struct GattLinkAcceptRunner<'s, 'a, M = NoopRawMutex>
where
    M: RawMutex,
{
    /// Active connection.
    conn: Connection<'s, DefaultPacketPool>,
    /// Incoming channel.
    /// The data on this channel is sent by the remote device via confirmed writes to characteristic `RX_CHAR_UUID`.
    incoming: Sender<'a, M, Payload>,
    /// Outgoing channel.
    /// The data on this channel is sent to the remote device via indications on characteristic `TX_CHAR_UUID`.
    outgoing: Receiver<'a, M, Payload>,
}

impl<M> GattLinkAcceptRunner<'_, '_, M>
where
    M: RawMutex,
{
    /// Run the GATT link runner.
    pub async fn run(mut self) -> Result<(), trouble_host::Error> {
        let server = unwrap!(Server::new_with_config(GapConfig::Peripheral(
            PeripheralConfig {
                name: "Zenoh",                                               // TODO
                appearance: &appearance::power_device::GENERIC_POWER_DEVICE, // TODO
            }
        )));

        let conn = self.conn.with_attribute_server(&server.server)?;

        let outgoing_confirmed = Signal::new();

        let mut incoming = pin!(Self::handle_incoming(
            &server,
            &conn,
            &mut self.incoming,
            &outgoing_confirmed
        ));
        let mut outgoing = pin!(Self::handle_outgoing(
            &server,
            &conn,
            &mut self.outgoing,
            &outgoing_confirmed
        ));

        match select(&mut incoming, &mut outgoing).await {
            Either::First(res) => res,
            Either::Second(res) => res,
        }
    }

    /// Handle incoming events.
    ///
    /// # Arguments
    /// - `server`: The GATT server.
    /// - `conn`: The GATT connection.
    /// - `incoming`: The sender side of the incoming channel.
    /// - `outgoing_confirmed`: The signal to notify when an indication is confirmed.
    async fn handle_incoming(
        server: &Server<'_>,
        conn: &GattConnection<'_, '_, DefaultPacketPool>,
        incoming: &mut Sender<'_, M, Payload>,
        outgoing_confirmed: &Signal<M, ()>,
    ) -> Result<(), trouble_host::Error> {
        debug!("GATT: Starting incoming handler");

        loop {
            match conn.next().await {
                GattConnectionEvent::Disconnected { reason } => {
                    info!("GATT: Disconnect {:?}", reason);

                    Err(trouble_host::Error::Disconnected)?;
                }
                GattConnectionEvent::Gatt { event } => {
                    let mut write_reply = false;

                    match event.payload().incoming() {
                        AttClient::Request(AttReq::Write {
                            handle,
                            data: bytes,
                        })
                        | AttClient::Command(AttCmd::Write {
                            handle,
                            data: bytes,
                        }) => {
                            if handle == server.zenoh_service.rx.handle {
                                trace!(
                                    "GATT: C1 Write {} len {} / MTU {}",
                                    Bytes(bytes),
                                    bytes.len(),
                                    conn.raw().att_mtu()
                                );

                                let payload = incoming.send().await;

                                payload.set(bytes);

                                incoming.send_done();

                                write_reply =
                                    matches!(event.payload().incoming(), AttClient::Request(_));

                                trace!("GATT: Data sent in the RX queue");
                            } else if Some(handle) == server.zenoh_service.tx.cccd_handle {
                                let subscribed = bytes[0] != 0;

                                trace!("GATT: Write to C2 CCC descriptor: {}", subscribed);

                                if !subscribed {
                                    Err(trouble_host::Error::Disconnected)?;
                                }

                                write_reply =
                                    matches!(event.payload().incoming(), AttClient::Request(_));
                            }
                        }
                        AttClient::Confirmation(AttCfm::ConfirmIndication) => {
                            outgoing_confirmed.signal(());

                            trace!("GATT: Confirm indication");

                            continue;
                        }
                        _ => (),
                    }

                    if write_reply {
                        event.into_payload().reply(AttRsp::Write).await?;
                    } else {
                        match event.accept() {
                            Ok(reply) => {
                                reply.send().await;
                            }
                            Err(e) => {
                                warn!("GATT: Error accepting event: {:?}", e);
                            }
                        }
                    }
                }
                _ => (),
            }
        }
    }

    /// Handle outgoing indications.
    ///
    /// # Arguments
    /// - `server`: The GATT server.
    /// - `conn`: The GATT connection.
    /// - `outgoing`: The receiver side of the outgoing channel.
    /// - `outgoing_confirmed`: The signal to wait for confirmation of indications.
    async fn handle_outgoing(
        server: &Server<'_>,
        conn: &GattConnection<'_, '_, DefaultPacketPool>,
        outgoing: &mut Receiver<'_, M, Payload>,
        outgoing_confirmed: &Signal<M, ()>,
    ) -> Result<(), trouble_host::Error> {
        debug!("GATT: Starting outgoing handler");

        loop {
            let payload = outgoing.receive().await;

            trace!(
                "GATT: Sending indication {} len {}",
                Bytes(payload.get()),
                payload.get().len()
            );

            GattData::send_unsolicited(
                conn.raw(),
                AttUns::Indicate {
                    handle: server.zenoh_service.tx.handle,
                    data: payload.get(),
                },
            )
            .await?;

            trace!(
                "GATT: Indicate {} len {}",
                Bytes(payload.get()),
                payload.get().len()
            );

            outgoing_confirmed.wait().await;

            outgoing.receive_done();

            trace!("GATT: Indication sent");
        }
    }
}

/// A hack to define characteristics which do not persist their
/// value in the proc-macro-defined Trouble Attribute Server.
///
/// We don't need to store any data on the Attribute Server
/// because the Zenoh GATT server in fact emulates a bidirectional
/// pipe over the GATT characteristics.
type External = [u8; 0];

// Zenoh GATT Server definition
#[gatt_server]
struct Server {
    zenoh_service: ZenohService,
}

/// Zenoh GATT Service
#[gatt_service(uuid = SERVICE_UUID)]
struct ZenohService {
    /// RX Characteristic - Write only, with optional confirmation
    #[characteristic(uuid = RX_CHAR_UUID, write)]
    rx: External,
    /// TX Characteristic - Indicate
    #[characteristic(uuid = TX_CHAR_UUID, write, indicate)]
    tx: External,
}
