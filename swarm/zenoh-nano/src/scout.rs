//! An implementation of the Zenoh Scouting protocol.

#![allow(async_fn_in_trait)]

use core::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use core::pin::pin;

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Timer};

use zenoh_buffers::reader::HasReader;
use zenoh_buffers::writer::HasWriter;
use zenoh_buffers::{BBuf, ZSlice};
use zenoh_codec::{RCodec, WCodec, Zenoh080};

use crate::link::{LinkError, LinkReceive, LinkSend};
use crate::transport::TransportError;

pub use zenoh_protocol::core::{Locator, WhatAmI, WhatAmIMatcher, ZenohIdProto};
pub use zenoh_protocol::scouting::*;

/// UDP Maximum Transmission Unit
///
/// With jumbo frames, this could be much larger, but then it is not expected that
/// the scout replies would be that large anyway.
pub const SCOUT_MTU: u16 = 1500;

/// The UDP multicast IP address used for scouting messages
pub const SCOUT_BROADCAST_IP_ADDR: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 224);

/// The UDP multicast socket port used for scouting messages
pub const SCOUT_BROADCAST_PORT: u16 = 7446;

/// The UDP multicast socket address used for scouting messages
pub const SCOUT_BROADCAST_SOCKET_ADDR: SocketAddr = SocketAddr::V4(SocketAddrV4::new(
    SCOUT_BROADCAST_IP_ADDR,
    SCOUT_BROADCAST_PORT,
));

/// Maximum seconds between scout messages
const MAX_SCOUT_SECS: u64 = 16;

/// Scout backoff factor
const SCOUT_BACKOFF_FACTOR: u64 = 2;

/// A variation of the `LinkReceive` trait that also provides the remote socket address of the sender
///
/// This trait can only be implemented over the UDP transport and is therefore
/// only useful when implementing the Zenoh Scouting protocol.
pub trait ScoutLinkReceive {
    /// Receive a binary message payload from the link, along with the remote address of the sender
    ///
    /// Returns:
    /// - `Ok((SocketAddr, ZSlice))`: The remote address and the received payload
    /// - `Err(LinkError)`: An error occurred during receiving
    async fn receive(&mut self) -> Result<(SocketAddr, ZSlice), LinkError>;
}

impl<T> ScoutLinkReceive for &mut T
where
    T: ScoutLinkReceive,
{
    async fn receive(&mut self) -> Result<(SocketAddr, ZSlice), LinkError> {
        (**self).receive().await
    }
}

/// A variation of the `LinkSend` trait that sends to a specific socket address of the recipient
///
/// This trait can only be implemented over the UDP transport and is therefore
/// only useful when implementing the Zenoh Scouting protocol.
pub trait ScoutLinkSend {
    /// Send a binary message payload over the link to the specified remote address
    ///
    /// Arguments:
    /// - `payload`: The binary message payload to send
    /// - `addr`: The remote socket address of the recipient
    ///
    /// Returns:
    /// - `Ok(())`: The message was sent successfully
    /// - `Err(LinkError)`: An error occurred during sending
    async fn send(&mut self, addr: &SocketAddr, payload: ZSlice) -> Result<(), LinkError>;
}

impl<T> ScoutLinkSend for &mut T
where
    T: ScoutLinkSend,
{
    async fn send(&mut self, addr: &SocketAddr, payload: ZSlice) -> Result<(), LinkError> {
        (**self).send(addr, payload).await
    }
}

/// Adapt a `ScoutLinkReceive` into a `LinkReceive`, filtering by remote address
pub struct LinkReceiveAdaptor<T, F> {
    /// The underlying scout link receiver
    recv: T,
    /// The remote address filter
    remote_addr_filter: F,
}

impl<T, F> LinkReceiveAdaptor<T, F> {
    /// Create a new `LinkReceiveAdaptor`
    ///
    /// Arguments:
    /// - `recv`: The underlying scout link receiver
    /// - `remote_addr_filter`: The remote address filter function
    pub const fn new(recv: T, remote_addr_filter: F) -> Self {
        Self {
            recv,
            remote_addr_filter,
        }
    }
}

impl<T, F> LinkReceive for LinkReceiveAdaptor<T, F>
where
    T: ScoutLinkReceive,
    F: FnMut(&SocketAddr) -> bool,
{
    fn mtu(&self) -> u16 {
        SCOUT_MTU
    }

    async fn receive(&mut self) -> Result<ZSlice, LinkError> {
        loop {
            let (remote_addr, payload) = self.recv.receive().await?;

            if (self.remote_addr_filter)(&remote_addr) {
                break Ok(payload);
            }
        }
    }
}

/// Adapt a `ScoutLinkSend` into a `LinkSend`, sending to a fixed remote address
pub struct LinkSendAdaptor<T> {
    /// The underlying scout link sender
    send: T,
    /// The fixed remote address
    addr: SocketAddr,
}

impl<T> LinkSendAdaptor<T> {
    /// Create a new `LinkSendAdaptor`
    ///
    /// Arguments:
    /// - `send`: The underlying scout link sender
    /// - `addr`: The fixed remote address
    pub const fn new(send: T, addr: SocketAddr) -> Self {
        Self { send, addr }
    }
}

impl<T> LinkSend for LinkSendAdaptor<T>
where
    T: ScoutLinkSend,
{
    fn mtu(&self) -> u16 {
        SCOUT_MTU
    }

    async fn send(&mut self, payload: ZSlice) -> Result<(), LinkError> {
        self.send.send(&self.addr, payload).await
    }
}

/// Runs the scouting protocol, by sending scouting messages and replying to scouting messages from other network peers.
/// This function will run until the provided callback returns `true`.
///
/// # Arguments
/// - `recv`: The scout link receiver
/// - `send`: The scout link sender
/// - `scout_what`: The `WhatAmIMatcher` indicating what kind of peers to scout for. If empty, no scouting messages will be sent.
/// - `me`: An optional `HelloProto` message to reply with when receiving a matching `Scout` message.
/// - `replies_callback`: A callback function that will be called when a `HelloProto` message is received.
///
/// # Returns
/// - `TransportError`: An error occurred during the scouting protocol execution.
pub async fn run<R, S, F>(
    mut receive: R,
    send: S,
    scout_what: WhatAmIMatcher,
    me: Option<HelloProto>,
    mut replies_callback: F,
) -> Result<(), TransportError>
where
    R: ScoutLinkReceive,
    S: ScoutLinkSend,
    F: FnMut(&SocketAddr, &HelloProto) -> bool,
{
    let send = Mutex::<NoopRawMutex, _>::new(send);

    let mut receive_task = pin!(async {
        loop {
            let mut remote_addr = None;

            let body = self::receive(LinkReceiveAdaptor::new(
                &mut receive,
                |addr: &SocketAddr| {
                    remote_addr = Some(*addr);

                    true
                },
            ))
            .await?;

            let remote_addr = unwrap!(remote_addr);

            match body {
                ScoutingBody::Scout(scout) => {
                    if let Some(me) = me.as_ref() {
                        debug!("Got scout request from {:?}: {:?}", remote_addr, scout);

                        if scout.what.matches(me.whatami) {
                            let mut send = send.lock().await;

                            debug!(
                                "Replying to scout request from {:?} with hello: {:?}",
                                remote_addr, me
                            );

                            self::send(
                                LinkSendAdaptor::new(&mut *send, remote_addr),
                                ScoutingBody::Hello(me.clone()),
                            )
                            .await?;
                        }
                    }
                }
                ScoutingBody::Hello(hello) => {
                    debug!("Got hello reply from {:?}: {:?}", remote_addr, hello);

                    if replies_callback(&remote_addr, &hello) {
                        break Ok(());
                    }
                }
            }
        }
    });

    let mut send_task = pin!(async {
        let mut secs = 1;

        loop {
            if scout_what.is_empty() {
                // Do nothing
                core::future::pending::<()>().await;
            } else {
                // Scout

                let scout = Scout {
                    version: zenoh_protocol::VERSION,
                    what: scout_what,
                    zid: None,
                };

                {
                    let mut send = send.lock().await;

                    debug!("Sending scout request: {:?}", scout);

                    self::send(
                        LinkSendAdaptor::new(&mut *send, SCOUT_BROADCAST_SOCKET_ADDR),
                        ScoutingBody::Scout(scout),
                    )
                    .await?;
                }

                Timer::after(Duration::from_secs(secs)).await;

                secs = (secs * SCOUT_BACKOFF_FACTOR).min(MAX_SCOUT_SECS);
            }
        }
    });

    match select(&mut receive_task, &mut send_task).await {
        Either::First(res) => res,
        Either::Second(res) => res,
    }
}

/// Receive a scouting message
///
/// # Arguments
/// - `recv`: The link receiver
///   Typically, the link receiver should be the reader half of a UDP socket.
///   Moreover, for receiving of `Scout` messages specifically, the socket _must_ be configured
///   in UDP multicast mode, by joining the 224.0.0.224 UDP multicast group.
///
/// # Returns
/// - `Ok(ScoutingBody)`: The received scouting message body
/// - `Err(TransportError)`: An error occurred during receiving or decoding
pub async fn receive<T>(mut receive: T) -> Result<ScoutingBody, TransportError>
where
    T: LinkReceive,
{
    let mut zslice = receive.receive().await?;

    let codec = Zenoh080::new();
    let msg: ScoutingMessage = codec
        .read(&mut zslice.reader())
        .map_err(|_| TransportError::IncomingMessageInvalid)?;

    Ok(msg.body)
}

/// Send a scouting message
///
/// # Arguments
/// - `send`: The link sender
///   Typically, the link sender should be the writer half of a UDP socket.
///   Moreover, for sending of `Scout` messages specifically, the socket _must_ be configured
///   in UDP multicast mode, by joining the 224.0.0.224 UDP multicast group.
/// - `scouting`: The scouting message body to send
///
/// # Returns
/// - `Ok(())`: The message was sent successfully
/// - `Err(TransportError)`: An error occurred during encoding or sending
pub async fn send<T>(mut send: T, scouting: ScoutingBody) -> Result<(), TransportError>
where
    T: LinkSend,
{
    let mut buf = BBuf::with_capacity(send.mtu() as usize);

    let codec = Zenoh080::new();
    let msg: ScoutingMessage = scouting.into();

    codec
        .write(&mut buf.writer(), &msg)
        .map_err(|_| TransportError::OutgoingMessageEncoding)?;

    send.send(buf.into()).await?;

    Ok(())
}
