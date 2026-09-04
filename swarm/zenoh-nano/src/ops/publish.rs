//! Publish Operation

use alloc::vec;

use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::pubsub::DynPublisher;

use zenoh_buffers::ZBuf;
use zenoh_protocol::core::{Encoding, Reliability, WireExpr};
use zenoh_protocol::network::declare::ext::NodeIdType;
use zenoh_protocol::network::ext::QoSType as NQoSType;
use zenoh_protocol::network::{Declare, DeclareBody, DeclareKeyExpr, Mapping, NetworkBody, Push};
use zenoh_protocol::zenoh::{PushBody, Put};

use crate::clock::Clock;
use crate::network::OutgoingMessage;
use crate::session::{Session, SessionError};

/// Controls what is the highest number of publishers we can have
pub const MAX_PUBLISHERS: usize = 10;

/// Zenoh Publisher
pub struct Publisher<'a, S> {
    publisher: DynPublisher<'a, OutgoingMessage>,
    /// The key onto which data is published
    key: S,
    /// The session's clock, captured at declare time. Stamps outgoing puts.
    clock: Option<&'static dyn Clock>,
}

impl<'a, S: AsRef<str>> Publisher<'a, S> {
    /// Declare the publisher to the Network
    pub async fn declare<M: RawMutex>(
        session: Session<'a, M>,
        key: S,
    ) -> Result<Self, SessionError> {
        debug!("Publisher declare {}", key.as_ref());

        let publisher = session.publisher()?;

        // Declare a new key expression so that we can publish data to it later
        publisher
            .publish(OutgoingMessage {
                body: NetworkBody::Declare(Declare {
                    interest_id: None,
                    ext_qos: NQoSType::DECLARE,
                    ext_tstamp: None,
                    ext_nodeid: NodeIdType::DEFAULT,
                    body: DeclareBody::DeclareKeyExpr(DeclareKeyExpr {
                        id: session.get_new_eid(),
                        wire_expr: WireExpr::empty().with_suffix(key.as_ref()).to_owned(),
                    }),
                }),
                reliability: Reliability::Reliable,
            })
            .await;

        Ok(Self {
            publisher,
            key,
            clock: session.clock(),
        })
    }

    /// Publish data
    pub async fn publish(&mut self, bytes: ZBuf) -> Result<(), SessionError> {
        trace!("Publisher publish data");

        self.publisher
            .publish(OutgoingMessage {
                body: NetworkBody::Push(Push {
                    wire_expr: {
                        let mut wire = WireExpr::empty().with_suffix(self.key.as_ref());
                        wire.mapping = Mapping::Receiver;
                        wire.to_owned()
                    },
                    ext_qos: NQoSType::PUSH,
                    ext_tstamp: None,
                    ext_nodeid: NodeIdType::DEFAULT,
                    payload: PushBody::Put(Put {
                        timestamp: self.clock.and_then(Clock::timestamp),
                        encoding: {
                            let mut enc = Encoding::empty();
                            // TODO: Figure out what this is
                            enc.id = 4;
                            enc
                        },
                        ext_sinfo: None,
                        ext_attachment: None,
                        ext_unknown: vec![],
                        payload: bytes,
                    }),
                }),
                reliability: Reliability::Reliable,
            })
            .await;

        Ok(())
    }
}
