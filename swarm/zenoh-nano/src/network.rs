//! Network

use core::sync::atomic::Ordering;

use alloc::vec;
use alloc::vec::Vec;

use embassy_time::Duration;
use portable_atomic::AtomicU32;

use zenoh_buffers::ZBuf;
use zenoh_buffers::reader::HasReader as _;
use zenoh_codec::{RCodec as _, Zenoh080};
use zenoh_protocol::core::{Reliability, ZenohIdProto};
use zenoh_protocol::network::NetworkMessage;
use zenoh_protocol::transport::{
    Close, Fragment, Frame, TransportBody, TransportMessage, ext::QoSType,
};

use crate::link::{LinkReceive, LinkSend};
use crate::rng::RandomSource;
use crate::transport::{SessionConfig, Transport, TransportError, TransportReceive, TransportSend};

/// Incoming message received from the network
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum IncomingMessage {
    /// Close request
    Close(Close),
    /// Regular message
    Message(NetworkMessage),
}

/// Outgoing message to be sent to the network
pub(crate) type OutgoingMessage = NetworkMessage;

/// Network error
#[derive(thiserror::Error, Debug, Copy, Clone, Eq, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum NetworkError {
    /// Error encoding outgoing message
    #[error("error encoding outgoing message")]
    OutgoingMessageEncoding,
    /// Transport error
    #[error("transport error: {0}")]
    Transport(#[from] TransportError),
}

/// State of the receiver
#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
enum ReceiveState {
    /// Clean
    None,
    /// Processing a frame
    Frame { payload: Vec<NetworkMessage> },
    /// Accumulating fragments
    Fragments { fragments: ZBuf },
}

/// The receive half of the network
pub struct NetworkReceive<R> {
    /// Underlying transport receiver
    pub(crate) receive: TransportReceive<R>,
    /// Current state
    state: ReceiveState,
}

impl<R: LinkReceive> NetworkReceive<R> {
    /// Create a new network receiver
    ///
    /// # Arguments
    /// - `receive`: The underlying transport receiver
    #[must_use]
    pub const fn new(receive: TransportReceive<R>) -> Self {
        Self {
            receive,
            state: ReceiveState::None,
        }
    }

    /// Receive a network message from the underlying transport
    ///
    /// This will block until a message is received.
    ///
    /// Note that the future returned by this method is cancellation-safe
    /// _only_ if the underlying link's `receive` method is cancellation-safe.
    /// Typically, streaming-based links (e.g., TCP, TLS) are NOT cancellation-safe,
    ///
    /// # Arguments
    /// - `config`: The session configuration received when initializing the transport
    ///
    /// # Returns
    /// - `Ok(IncomingMessage)`: The received message
    /// - `Err(NetworkError)`: An error occurred
    pub async fn receive(
        &mut self,
        config: &SessionConfig,
    ) -> Result<IncomingMessage, NetworkError> {
        loop {
            if let ReceiveState::Frame { payload } = &mut self.state {
                if !payload.is_empty() {
                    let message = IncomingMessage::Message(payload.remove(0));
                    if payload.is_empty() {
                        self.state = ReceiveState::None;
                    }

                    break Ok(message);
                } else {
                    self.state = ReceiveState::None;
                }
            }

            let msg = self.receive.receive().await?;

            match msg.body {
                TransportBody::Frame(Frame { payload, .. }) => {
                    if !matches!(self.state, ReceiveState::None) {
                        error!(
                            "Received Frame while already processing another message: {:?}; dropping previous message",
                            self.state
                        );
                    }

                    self.state = ReceiveState::Frame { payload };
                    continue;
                }
                TransportBody::Fragment(Fragment {
                    more,
                    ext_drop,
                    payload,
                    ..
                }) => {
                    let state = core::mem::replace(&mut self.state, ReceiveState::None);

                    let mut fragments = if let ReceiveState::Fragments { fragments } = state {
                        fragments
                    } else {
                        ZBuf::empty()
                    };

                    let patch = config.patch;

                    // Check if the current protocol patch version can give us hints with
                    // markers to help processing the reassembly
                    if patch.has_fragmentation_markers() && ext_drop.is_some() {
                        // Discard reassembly and drop message
                        fragments.clear();
                    } else {
                        fragments.push_zslice(payload);
                    }

                    if !more {
                        let mut reader = fragments.reader();
                        let codec = Zenoh080::new();
                        match codec.read(&mut reader) {
                            Ok(network_message) => {
                                break Ok(IncomingMessage::Message(network_message));
                            }
                            Err(e) => {
                                warn!("Attempt to reassemble fragmented message failed: {:?}", e);
                                continue;
                            }
                        }
                    } else {
                        self.state = ReceiveState::Fragments { fragments };
                        continue;
                    }
                }
                TransportBody::Close(close) => {
                    break Ok(IncomingMessage::Close(close));
                }
                _ => {
                    // Ignore other messages
                    continue;
                }
            }
        }
    }
}

/// The send half of the network
pub struct NetworkSend<S>(pub(crate) TransportSend<S>);

impl<S: LinkSend> NetworkSend<S> {
    /// Create a new network sender
    ///
    /// # Arguments
    /// - `send`: The underlying transport sender
    #[must_use]
    pub const fn new(send: TransportSend<S>) -> Self {
        Self(send)
    }

    /// Send a network message to the underlying transport
    ///
    /// This will block until the message is sent.
    ///
    /// Note that the future returned by this method is cancellation-safe
    /// _only_ if the underlying link's `send` method is cancellation-safe.
    /// Typically, streaming-based links (e.g., TCP, TLS) are NOT cancellation-safe,
    ///
    /// # Arguments
    /// - `config`: The session configuration received when initializing the transport
    /// - `sequence`: The atomic sequence number to use for the fragments of the message
    ///   if the message needs to be fragmented
    /// - `msg`: The message to send
    ///
    /// # Returns
    /// - `Ok(())`: The message was sent
    /// - `Err(NetworkError)`: An error occurred
    pub async fn send(
        &mut self,
        _config: &SessionConfig,
        sequence: &AtomicU32,
        msg: OutgoingMessage,
    ) -> Result<(), NetworkError> {
        // TODO: No batching, and not easy to do with the current architecture, as we only "see" one
        // network message rather than multiple

        let msg = TransportMessage {
            body: TransportBody::Frame(Frame {
                sn: sequence.fetch_add(1, Ordering::Relaxed),
                reliability: Reliability::Reliable,
                ext_qos: QoSType::DEFAULT,
                payload: vec![msg],
            }),
        };

        self.0.send(&msg, Some(sequence)).await?;

        Ok(())
    }
}

/// Network
///
/// The network is responsible for sending and receiving Zenoh network messages
/// (i.e. `NetworkMessage` instances) over the enclosed transport, by converting those from/to
/// Zenoh `TransportMessage` instances and then sending/receiving them using the enclosed transport.
pub struct Network<'a, R, S> {
    /// The receive half of the network
    pub(crate) receive: NetworkReceive<R>,
    /// The send half of the network
    pub(crate) send: NetworkSend<S>,
    /// The session configuration received when initializing the transport
    pub(crate) config: SessionConfig,
    /// A reference to the random number generator
    /// Needs to be around so that our custom getrandom global hook works
    _rng: RandomSource<'a>,
}

impl<'a, R: LinkReceive, S: LinkSend> Network<'a, R, S> {
    /// Establish a session to a peer
    ///
    /// See `Transport::connect` for details as this is just a thin wrapper around it.
    ///
    /// # Arguments
    /// - `receive`: The underlying link receiver
    /// - `send`: The underlying link sender
    /// - `lease`: The desired lease duration for the session
    /// - `rng`: A random number generator
    ///
    /// # Returns
    /// - `Ok(Network)`: The established network
    /// - `Err(NetworkError)`: An error occurred
    pub async fn connect(
        receive: R,
        send: S,
        lease: Duration,
        rng: RandomSource<'a>,
        our_zid: ZenohIdProto,
    ) -> Result<Self, NetworkError> {
        let transport = Transport::connect(receive, send, lease, our_zid).await?;

        Ok(Self {
            receive: NetworkReceive::new(transport.receive),
            send: NetworkSend::new(transport.send),
            config: transport.config,
            _rng: rng,
        })
    }

    /// Listen for a session establishment by a peer and attempt to complete it
    ///
    /// See `Transport::accept` for details as this is just a thin wrapper around it.
    ///
    /// # Arguments
    /// - `receive`: The underlying link receiver
    /// - `send`: The underlying link sender
    /// - `lease`: The desired lease duration for the session
    /// - `rng`: A random number generator
    ///
    /// # Returns
    /// - `Ok(Network)`: The established network
    /// - `Err(NetworkError)`: An error occurred
    pub async fn accept(
        receive: R,
        send: S,
        lease: Duration,
        rng: RandomSource<'a>,
        our_zid: ZenohIdProto,
    ) -> Result<Self, TransportError> {
        let transport = Transport::accept(receive, send, lease, &rng, our_zid).await?;

        Ok(Self {
            receive: NetworkReceive::new(transport.receive),
            send: NetworkSend::new(transport.send),
            config: transport.config,
            _rng: rng,
        })
    }
}
