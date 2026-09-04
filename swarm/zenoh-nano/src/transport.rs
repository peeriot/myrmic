//! Transport

use core::sync::atomic::Ordering;

use embassy_time::{Duration, TimeoutError, with_timeout};

use portable_atomic::AtomicU32;

use zenoh_buffers::buffer::Buffer;
use zenoh_buffers::reader::{HasReader as _, Reader as _};
use zenoh_buffers::writer::HasWriter as _;
use zenoh_buffers::{BBuf, ZBuf, ZSlice};
use zenoh_codec::transport::batch::Zenoh080Batch;
use zenoh_codec::{RCodec as _, WCodec as _, Zenoh080};
use zenoh_protocol::VERSION;
use zenoh_protocol::core::{Resolution, WhatAmI, ZenohIdProto};
use zenoh_protocol::transport::fragment;
use zenoh_protocol::transport::init::ext::{Auth, PatchType};
use zenoh_protocol::transport::{
    FragmentHeader, InitAck, InitSyn, OpenAck, OpenSyn, TransportBody, TransportMessage,
};

use crate::link::{LinkError, LinkReceive, LinkSend};
use crate::rng::RandomSource;

/// Session configuration
///
/// This configuration is established during the session negotiation phase
/// and is then used for the lifetime of the session.
#[derive(Copy, Clone, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct SessionConfig {
    pub(crate) our_zid: ZenohIdProto,
    pub(crate) peer_zid: ZenohIdProto,
    pub(crate) lease: Duration,
    pub(crate) patch: PatchType,
    pub(crate) initial_sn: u32,
}

/// Transport error
#[derive(thiserror::Error, Debug, Copy, Clone, Eq, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum TransportError {
    /// Timeout while acquiring random data
    #[error("timeout while acquiring random data")]
    RngTimeout,
    /// Timeout during session negotiation
    #[error("timeout during session negotiation")]
    SessionNegoTimeout,
    /// Invalid response during session negotiation
    #[error("invalid response during session negotiation")]
    SessionNegoInvalidResponse,
    /// Incoming message is invalid
    #[error("incoming message is invalid")]
    IncomingMessageInvalid,
    /// Outgoing message is too large and does not fit the link MTU
    #[error("outgoing message is too large")]
    OutgoingMessageTooLarge,
    /// Error encoding outgoing message
    #[error("error encoding outgoing message")]
    OutgoingMessageEncoding,
    /// Link error
    #[error("link error: {0}")]
    Link(#[from] LinkError),
}

impl From<TimeoutError> for TransportError {
    fn from(_: TimeoutError) -> Self {
        Self::SessionNegoTimeout
    }
}

/// The receive half of the transport
pub struct TransportReceive<R> {
    receive: R,
    negotiated_mtu: u16,
}

impl<R> TransportReceive<R> {
    /// Create a new TransportReceive
    const fn new(receive: R, negotiated_mtu: u16) -> Self {
        Self {
            receive,
            negotiated_mtu,
        }
    }
}

impl<R: LinkReceive> TransportReceive<R> {
    /// Receive a transport message from the link
    ///
    /// This will block until a message is received.
    ///
    /// Note that the future returned by this method is cancellation-safe
    /// _only_ if the underlying link's `receive` method is cancellation-safe.
    /// Typically, streaming-based links (e.g., TCP, TLS) are NOT cancellation-safe,
    ///
    /// # Returns
    /// - Ok(TransportMessage) if a message was successfully received and decoded
    /// - Err(TransportError) if an error occurred during reception or decoding
    pub async fn receive(&mut self) -> Result<TransportMessage, TransportError> {
        let max_payload_size = (self.negotiated_mtu - self.receive.mtu_header_size()) as usize;

        let mut data = self.receive.receive().await?;

        if data.len() > max_payload_size as _ {
            return Err(TransportError::IncomingMessageInvalid);
        }

        // Decode
        let codec = Zenoh080::new();

        let mut reader = data.reader();

        let msg: TransportMessage = codec
            .read(&mut reader)
            .map_err(|_| TransportError::IncomingMessageInvalid)?;

        debug!("Received transport message: {:?}", msg);

        Ok(msg)
    }
}

/// The send half of the transport
pub struct TransportSend<S> {
    send: S,
    negotiated_mtu: u16,
}

impl<R> TransportSend<R> {
    /// Create a new TransportSend
    const fn new(send: R, negotiated_mtu: u16) -> Self {
        Self {
            send,
            negotiated_mtu,
        }
    }
}

impl<S: LinkSend> TransportSend<S> {
    /// Send a transport message to the link
    ///
    /// This will block until the message is sent.
    ///
    /// Note that the future returned by this method is cancellation-safe
    /// _only_ if the underlying link's `send` method is cancellation-safe.
    /// Typically, streaming-based links (e.g., TCP, TLS) are NOT cancellation-safe,
    ///
    /// # Arguments
    /// - msg: The transport message to send
    /// - fragment_sequence: An optional atomic sequence number to use for fragmentation.
    ///   If not provided, messages that require fragmentation will result in an error.
    ///
    /// # Returns
    /// - Ok(()) if the message was successfully sent
    /// - Err(TransportError) if an error occurred during encoding or sending
    pub async fn send(
        &mut self,
        msg: &TransportMessage,
        fragment_sequence: Option<&AtomicU32>,
    ) -> Result<(), TransportError> {
        // TODO: Think again whether `msg: TransportMessage` is the correct signature here
        // It implies that the `Network` type is aware of transport messages, which is not ideal
        //
        // Furthermore, the fragmentation-upon-receive is also done in the `Network` type, while
        // ideally it should be done here, in the transport, because it is a transport notion
        //
        // Ideally, we should deal with transport messages **only** in this layer,
        // and the upper layers should always only see network messages.
        //
        // This likely means retiring the `Network` type, but that's actually not bad -
        // less notions for the user to deal with. This would also help with cleaning up
        // the wrong sequences when doing fragmentation.

        debug!("About to send transport message: {}", msg);

        let max_payload_size = (self.negotiated_mtu - self.send.mtu_header_size()) as usize;

        let mut buf = BBuf::with_capacity(max_payload_size);

        if Zenoh080::new().write(&mut buf.writer(), msg).is_ok() {
            // Message is good to be sent (no fragmentation needed)
            self.send.send(buf.into()).await?;
        } else if let TransportBody::Frame(frame) = &msg.body {
            // Message doesn't fit in batch (needs fragmentation)
            trace!("Transport message (Frame) too large, fragmenting its network messages");

            let Some(sequence) = fragment_sequence else {
                return Err(TransportError::OutgoingMessageTooLarge);
            };

            for (index, msg) in frame.payload.iter().enumerate() {
                trace!(
                    "Fragmenting {}/{} network message",
                    index + 1,
                    frame.payload.len()
                );

                let mut header = FragmentHeader {
                    reliability: frame.reliability,
                    more: true,
                    // TODO: See above
                    // The fact that `Network` deals with sequence numbers is totally NOK
                    sn: frame.sn,
                    ext_qos: frame.ext_qos,
                    ext_first: Some(fragment::ext::First::new()),
                    ext_drop: None,
                };

                let mut zbuf = ZBuf::empty();

                Zenoh080::new()
                    .write(&mut zbuf.writer(), msg)
                    .map_err(|_| TransportError::OutgoingMessageEncoding)?;

                trace!("Network message size: {}", zbuf.len());

                let mut reader = zbuf.reader();

                // Fragmentation
                let mut codec = Zenoh080Batch::new();
                while header.more {
                    header.sn = sequence.fetch_add(1, Ordering::Relaxed);

                    let mut buf = BBuf::with_capacity(max_payload_size);

                    // Pass the reader and the Fragment Header so that the codec can handle it for us
                    codec
                        .write(&mut buf.writer(), (&mut reader, &mut header))
                        .map_err(|_| TransportError::OutgoingMessageEncoding)?;

                    debug!("Fragment size: {}", buf.len());

                    self.send.send(buf.into()).await?;

                    header.ext_first = None;
                }

                assert!(!reader.can_read());
            }
        } else {
            return Err(TransportError::OutgoingMessageTooLarge);
        }

        debug!("Sent transport message: {}", msg);

        Ok(())
    }
}

/// Transport
///
/// The transport is responsible for sending and receiving Zenoh transport messages
/// (i.e. `TransportMessage` instances) over a given link by converting those from/to
/// byte sequences and then sending/receiving them over the enclosed link.
pub struct Transport<R, S> {
    /// The receive half of the transport
    pub(crate) receive: TransportReceive<R>,
    /// The send half of the transport
    pub(crate) send: TransportSend<S>,
    /// The session configuration established during session negotiation
    pub(crate) config: SessionConfig,
}

impl<R: LinkReceive, S: LinkSend> Transport<R, S> {
    /// Establish a session to a peer
    ///
    /// For some transports, the embedded device needs to be the initiator of a session, in other terms
    /// it's the device that has to "reach out" (e.g. connecting to Cloud). In those cases we should be
    /// the initiators of the session establishment. It works the same way as it works when we are the
    /// listener, only that the messages are flowing in the reverse direction.
    ///
    /// Session establishment comms:
    ///
    /// Client              Router
    /// |    `InitSyn`        |
    /// |-------------------->|
    /// |                     |
    /// |`InitAck` \[cookie\] |
    /// |<--------------------|
    /// |                     |
    /// |`OpenSyn` \[cookie\] |
    /// |-------------------->|
    /// |                     |
    /// |    `OpenAck`        |
    /// |<--------------------|
    /// ~        ...          ~
    /// |                     |
    /// |    KEEP ALIVE       | (sent every 1/4th of `OpenSyn` lease time)
    /// |<--------------------|
    /// |                     |
    /// |    KEEP ALIVE       |
    /// |-------------------->|
    /// |                     |
    /// ~        ...          ~
    ///
    /// # Arguments
    /// - receive: The link receive half
    /// - send: The link send half
    /// - lease: The desired lease duration for the session
    ///
    /// # Returns
    /// - Ok(Transport) if the session was successfully established
    /// - Err(TransportError) if an error occurred during session establishment
    pub(crate) async fn connect(
        receive: R,
        send: S,
        lease: Duration,
        our_zid: ZenohIdProto,
    ) -> Result<Self, TransportError> {
        let link_mtu = receive.mtu().min(send.mtu());

        let mut receive = TransportReceive::new(receive, link_mtu);
        let mut send = TransportSend::new(send, link_mtu);

        // Initialize session
        let syn = TransportBody::InitSyn(InitSyn {
            version: VERSION,
            whatami: WhatAmI::Client,
            zid: our_zid,
            resolution: Resolution::default(),
            batch_size: link_mtu,
            ext_qos: None,
            ext_qos_link: None,
            ext_auth: None,
            ext_mlink: None,
            ext_lowlatency: None,
            ext_compression: None,
            ext_patch: PatchType::CURRENT,
            ext_region_name: None,
        });

        send.send(&syn.into(), None).await?;

        let response = with_timeout(Duration::from_secs(10), receive.receive()).await??;

        let TransportMessage {
            body:
                TransportBody::InitAck(InitAck {
                    zid: peer_zid,
                    cookie,
                    ext_patch,
                    batch_size,
                    ..
                }),
        } = response
        else {
            return Err(TransportError::SessionNegoInvalidResponse);
        };

        // FIXME: We need to abide to some rules when determining the sequence number. Look at:
        //        zenoh/io/zenoh-transport/seq_num.rs. For now we can use a fixed number
        let initial_sn = 1234;

        // Open session
        let syn = TransportBody::OpenSyn(OpenSyn {
            lease: lease.into(),
            initial_sn,
            cookie,
            ext_qos: None,
            ext_auth: None,
            ext_mlink: None,
            ext_lowlatency: None,
            ext_compression: None,
            ext_south: None,
        });

        send.send(&syn.into(), None).await?;

        let response = with_timeout(Duration::from_secs(10), receive.receive()).await??;

        let TransportMessage {
            body:
                TransportBody::OpenAck(OpenAck {
                    lease: remote_lease,
                    ..
                }),
        } = response
        else {
            return Err(TransportError::SessionNegoInvalidResponse);
        };

        let negotiated_mtu = batch_size.min(link_mtu);
        let lease = lease.min(
            remote_lease
                .try_into()
                .map_err(|_| TransportError::SessionNegoInvalidResponse)?,
        );

        let config = SessionConfig {
            our_zid,
            peer_zid,
            patch: ext_patch,
            initial_sn,
            lease,
        };

        info!(
            "Session established with config: {:?}, MTU: {}",
            config, negotiated_mtu
        );

        Ok(Self {
            receive: TransportReceive::new(receive.receive, negotiated_mtu),
            send: TransportSend::new(send.send, negotiated_mtu),
            config,
        })
    }

    /// Listen for a session establishment by a peer and attempt to complete it
    ///
    /// To open a session between Client (us) and Router, we can't use scouting. So this will use the
    /// unicast ability of session establishment to connect to the router. In the case of our embedded
    /// solution, we are a client that awaits a connection from the router (in zenoh terms, we offer a
    /// listening locator). This means it's up to the Router to initiate the session establishment and
    /// eventually retry in case of errors.
    ///
    /// Session establishment comms:
    ///
    /// Router            Client
    /// |    `InitSyn`        |
    /// |-------------------->|
    /// |                     |
    /// |`InitAck` \[cookie\] |
    /// |<--------------------|
    /// |                     |
    /// |`OpenSyn` \[cookie\] |
    /// |-------------------->|
    /// |                     |
    /// |    `OpenAck`        |
    /// |<--------------------|
    /// ~        ...          ~
    /// |                     |
    /// |    KEEP ALIVE       | (sent every 1/4th of `OpenSyn` lease time)
    /// |<--------------------|
    /// |                     |
    /// |    KEEP ALIVE       |
    /// |-------------------->|
    /// |                     |
    /// ~        ...          ~
    ///
    /// # Arguments
    /// - receive: The link receive half
    /// - send: The link send half
    /// - lease: The maximum desired lease duration for the session
    /// - rng: A random number generator to use for cookie generation
    ///
    /// # Returns
    /// - Ok(Transport) if the session was successfully established
    /// - Err(TransportError) if an error occurred during session establishment
    pub(crate) async fn accept(
        receive: R,
        send: S,
        lease: Duration,
        rng: &RandomSource<'_>,
        our_zid: ZenohIdProto,
    ) -> Result<Self, TransportError> {
        let link_mtu = receive.mtu().min(send.mtu());

        let mut receive = TransportReceive::new(receive, link_mtu);
        let mut send = TransportSend::new(send, link_mtu);

        // Create cookie
        let cookie: ZSlice = {
            let mut cookie_buf = [0u8; size_of::<u64>()];

            rng.fill_bytes(&mut cookie_buf);

            cookie_buf.into()
        };

        // Wait for incoming connection (indefinitely)
        let request = receive.receive().await?;

        let TransportMessage {
            body:
                TransportBody::InitSyn(InitSyn {
                    zid: peer_zid,
                    resolution,
                    batch_size,
                    ext_patch,
                    ..
                }),
        } = request
        else {
            return Err(TransportError::SessionNegoInvalidResponse);
        };

        let negotiated_mtu = batch_size.min(link_mtu);

        let ack = TransportBody::InitAck(InitAck {
            version: VERSION,
            whatami: WhatAmI::Client,
            zid: our_zid,
            resolution,
            batch_size: negotiated_mtu,
            cookie: cookie.clone(),
            ext_qos: None,
            ext_qos_link: None,
            ext_auth: Some(Auth::new(ZBuf::empty())),
            ext_mlink: None,
            ext_lowlatency: None,
            ext_compression: None,
            ext_patch,
            ext_region_name: None,
        });

        send.send(&ack.into(), None).await?;

        let response = with_timeout(Duration::from_secs(10), receive.receive()).await??;

        let TransportMessage {
            body:
                TransportBody::OpenSyn(OpenSyn {
                    lease: remote_lease,
                    initial_sn,
                    cookie: rx_cookie,
                    ..
                }),
        } = response
        else {
            return Err(TransportError::SessionNegoInvalidResponse);
        };

        if cookie != rx_cookie {
            return Err(TransportError::SessionNegoInvalidResponse);
        }

        let lease: Duration = remote_lease
            .min(lease.into())
            .try_into()
            .map_err(|_| TransportError::SessionNegoInvalidResponse)?;

        let ack = TransportBody::OpenAck(OpenAck {
            lease: lease.into(),
            initial_sn,
            ext_qos: None,
            ext_auth: None,
            ext_mlink: None,
            ext_lowlatency: None,
            ext_compression: None,
            ext_south: None,
        });

        send.send(&ack.into(), None).await?;

        let config = SessionConfig {
            our_zid,
            peer_zid,
            patch: ext_patch,
            lease,
            initial_sn,
        };

        info!(
            "Session established with config: {:?}, MTU: {}",
            config, negotiated_mtu
        );

        Ok(Self {
            receive: TransportReceive::new(receive.receive, negotiated_mtu),
            send: TransportSend::new(send.send, negotiated_mtu),
            config,
        })
    }
}
