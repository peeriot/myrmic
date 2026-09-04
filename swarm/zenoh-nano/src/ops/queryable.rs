//! Queryable operation

use alloc::string::String;
use alloc::vec;

use embassy_sync::blocking_mutex::raw::{NoopRawMutex, RawMutex};
use embassy_sync::channel::DynamicReceiver;
use embassy_sync::pubsub::DynPublisher;

use zenoh_buffers::ZBuf;
use zenoh_protocol::core::key_expr::OwnedKeyExpr;
use zenoh_protocol::core::{Encoding, Reliability, WireExpr};
use zenoh_protocol::network::declare::QueryableId;
use zenoh_protocol::network::declare::queryable::ext::QueryableInfoType;
use zenoh_protocol::network::ext::EntityGlobalIdType;
use zenoh_protocol::network::request::ext::{NodeIdType, QoSType as NQoSType};
use zenoh_protocol::network::{
    Declare, DeclareBody, DeclareQueryable, Mapping, NetworkBody, Response, ResponseFinal,
};
use zenoh_protocol::zenoh::reply::ReplyBody;
use zenoh_protocol::zenoh::{ConsolidationMode, Err, Put, Reply, ResponseBody};

use crate::clock::Clock;
use crate::dispatch::{Dispatch, Route, Routed};
use crate::network::OutgoingMessage;
use crate::session::{Session, SessionError};

/// Query Token
///
/// Can be used to execute a query and then build a reply
#[derive(Debug, Clone)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Query {
    /// Original ID of Request
    ///
    /// This needs to be contained in the reply so that the Response is connected to a Request
    request_id: u32,

    qos: NQoSType,

    /// Optionally included body data
    #[allow(dead_code)] // Doesn't have to be used
    pub body: Option<ZBuf>,
    /// Optionally included attachment
    #[allow(dead_code)] // Doesn't have to be used
    pub attachment: Option<ZBuf>,
}

/// Zenoh Queryable
pub struct Queryable<'a, M: RawMutex = NoopRawMutex> {
    session: Session<'a, M>,
    publisher: DynPublisher<'a, OutgoingMessage>,
    dispatch: &'a dyn Dispatch,
    slot: usize,
    receiver: DynamicReceiver<'a, Routed>,
    key_expr: OwnedKeyExpr,
}

impl<'a, M: RawMutex> Queryable<'a, M> {
    /// Declare the queryable to the Network
    pub async fn declare(
        session: Session<'a, M>,
        key: impl AsRef<str>,
    ) -> Result<Self, SessionError> {
        let key_expr = OwnedKeyExpr::new(key.as_ref()).unwrap();
        let qid = session.get_new_qid();

        debug!("Queryable declare on: {}", key_expr.as_str());

        let msg = build_declare_msg(qid, key_expr.as_str());

        let publisher = session.publisher()?;
        let dispatch = session.dispatch();
        let (slot, receiver) = session.register(Route::Queryable {
            key: String::from(key_expr.as_str()),
            redeclare: msg.clone(),
        })?;

        // Send the initial declare; re-declaration after a reconnect is handled centrally.
        publisher.publish(msg).await;

        Ok(Self {
            session,
            publisher,
            dispatch,
            slot,
            receiver,
            key_expr,
        })
    }

    /// Returns the queryable's key
    pub fn key(&self) -> String {
        String::from(self.key_expr.as_str())
    }

    /// Await query
    ///
    /// # Returns
    ///
    /// The query to be replied to
    pub async fn wait_for_query(&mut self) -> Result<Query, SessionError> {
        trace!("Awaiting query");

        // The dispatcher only ever routes query requests to a queryable slot. Any other variant is
        // an internal routing bug: panic in debug/test builds to catch it, and in release firmware
        // log an error and skip it rather than bricking the device.
        loop {
            match self.receiver.receive().await {
                Routed::Query {
                    request_id,
                    qos,
                    body,
                    attachment,
                } => {
                    return Ok(Query {
                        request_id,
                        qos,
                        body,
                        attachment,
                    });
                }
                _ => {
                    debug_assert!(false, "queryable slot received a non-Query routed message");
                    error!("BUG: queryable slot received a non-Query routed message; skipping");
                }
            }
        }
    }

    /// Replies to query with data
    pub async fn reply_to_query(
        &self,
        query: Query,
        result: Result<ZBuf, ZBuf>,
    ) -> Result<(), SessionError> {
        let payload = match result {
            Ok(payload) => {
                debug!(
                    "Replying to query for {} with Ok(_)",
                    self.key_expr.as_str()
                );

                ResponseBody::Reply(Reply {
                    consolidation: ConsolidationMode::DEFAULT,
                    ext_unknown: vec![],
                    payload: ReplyBody::Put(Put {
                        timestamp: self.session.clock().and_then(Clock::timestamp),
                        encoding: Encoding::empty(),
                        ext_sinfo: None,
                        ext_attachment: None,
                        ext_unknown: vec![],
                        payload,
                    }),
                })
            }
            Err(payload) => {
                debug!(
                    "Replying to query for {} with Err(_)",
                    self.key_expr.as_str()
                );

                ResponseBody::Err(Err {
                    encoding: Encoding::empty(),
                    ext_sinfo: None,
                    ext_unknown: vec![],
                    payload,
                })
            }
        };

        // Fetch our ZID fresh so replies carry the correct id even after a reconnect.
        let zid = self.session.zid().await;

        // Respond to query
        self.publisher
            .publish(OutgoingMessage {
                body: NetworkBody::Response(Response {
                    rid: query.request_id,
                    wire_expr: {
                        let mut wire = WireExpr::empty().with_suffix(self.key_expr.as_str());
                        wire.mapping = Mapping::Sender;
                        wire.to_owned()
                    },
                    payload,
                    ext_qos: query.qos,
                    ext_tstamp: None,
                    ext_respid: Some(EntityGlobalIdType {
                        zid,
                        // FIXME: Why is this important and how does it work?
                        eid: 0,
                    }),
                }),
                reliability: Reliability::Reliable,
            })
            .await;

        self.finish_query(query).await
    }

    /// Produces a final response without any data being returned
    ///
    /// # Warning
    ///
    /// This should be used only to reply to a query with no data (or to be used internally in this
    /// file).
    /// Using this in conjunction with `reply_to_query` will break Zenoh's logic.
    pub async fn finish_query(&self, query: Query) -> Result<(), SessionError> {
        // Signal that we don't have any more data to add to the response
        self.publisher
            .publish(OutgoingMessage {
                body: NetworkBody::ResponseFinal(ResponseFinal {
                    rid: query.request_id,
                    ext_qos: query.qos,
                    ext_tstamp: None,
                }),
                reliability: Reliability::Reliable,
            })
            .await;

        Ok(())
    }
}

impl<M: RawMutex> Drop for Queryable<'_, M> {
    fn drop(&mut self) {
        self.dispatch.unregister(self.slot);
    }
}

/// Builds the `DeclareQueryable` network message for `key`, tagged with the unique `qid` so that
/// multiple queryables on the same session don't collide at the router.
fn build_declare_msg(qid: QueryableId, key: &str) -> OutgoingMessage {
    OutgoingMessage {
        body: NetworkBody::Declare(Declare {
            interest_id: None,
            ext_qos: NQoSType::DECLARE,
            ext_tstamp: None,
            ext_nodeid: NodeIdType::DEFAULT,
            body: DeclareBody::DeclareQueryable(DeclareQueryable {
                id: qid,
                wire_expr: WireExpr::empty().with_suffix(key).to_owned(),
                ext_info: QueryableInfoType::default(),
            }),
        }),
        reliability: Reliability::Reliable,
    }
}
