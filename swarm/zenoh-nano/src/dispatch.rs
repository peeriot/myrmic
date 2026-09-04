//! Central inbound message dispatcher.
//!
//! Instead of broadcasting every received [`NetworkMessage`] to every consumer and having each one
//! re-filter (which wakes every task on every message and couples all consumers through one shared
//! queue), a single dispatcher owns a routing table and delivers each message only to the
//! consumer(s) that want it, through private per-consumer channels.
//!
//! Each active consumer ([`Subscriber`](crate::ops::subscribe::Subscriber),
//! [`Queryable`](crate::ops::queryable::Queryable), or in-flight
//! [`Get`](crate::ops::get::Get)) claims one slot in a [`SubscriberPool`]. The pool exposes a
//! type-erased [`Dispatch`] handle so that [`Session`](crate::session::Session) and the consumers
//! do not need to name the pool's const-generic sizes.

use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::{NoopRawMutex, RawMutex};
use embassy_sync::channel::{Channel, DynamicReceiver};

use zenoh_buffers::ZBuf;
use zenoh_protocol::network::request::ext::QoSType as RequestQoSType;
use zenoh_protocol::network::{
    NetworkBody, NetworkMessage, Push, Request, RequestId, Response, ResponseFinal,
};
use zenoh_protocol::zenoh::reply::ReplyBody;
use zenoh_protocol::zenoh::{Err as ZErr, PushBody, Put, Reply, RequestBody, ResponseBody};

/// Default number of consumer slots in a [`SubscriberPool`].
///
/// Shared by subscribers, queryables and in-flight gets.
pub const DEFAULT_SLOTS: usize = 20;

/// Default depth of each consumer's private channel.
pub const DEFAULT_SLOT_DEPTH: usize = 4;

/// Whether a push received on `incoming` belongs to a subscription declared on `declared`.
///
/// Falls back to exact equality when either side is not a valid keyexpr.
pub(crate) fn key_matches(declared: &str, incoming: &str) -> bool {
    use zenoh_protocol::core::key_expr::keyexpr;

    match (keyexpr::new(declared), keyexpr::new(incoming)) {
        (Ok(declared), Ok(incoming)) => declared.intersects(incoming),
        _ => declared == incoming,
    }
}

/// A consumer's registration in the dispatcher's routing table.
///
/// `redeclare` holds a clone of the `Declare` message the consumer sent when it declared itself, so
/// the dispatcher can re-send it verbatim after a reconnect.
pub enum Route {
    /// A subscriber matching pushes whose keyexpr intersects `key`.
    Subscriber {
        /// The declared keyexpr.
        key: String,
        /// The `DeclareSubscriber` message to re-send after a reconnect.
        redeclare: NetworkMessage,
    },
    /// A queryable matching requests whose keyexpr intersects `key`.
    Queryable {
        /// The declared keyexpr.
        key: String,
        /// The `DeclareQueryable` message to re-send after a reconnect.
        redeclare: NetworkMessage,
    },
    /// An in-flight get matching responses by request id. Never re-declared.
    Get {
        /// The request id whose responses this slot receives.
        request_id: RequestId,
    },
}

/// The payload delivered into a consumer's private channel.
pub enum Routed {
    /// A matched put for a subscriber (concrete keyexpr the message fired on, plus payload).
    Push {
        /// The concrete keyexpr the message was published on.
        key: String,
        /// The payload.
        payload: ZBuf,
    },
    /// A matched query request for a queryable.
    Query {
        /// Original request id, to be echoed in the reply.
        request_id: RequestId,
        /// QoS of the request, to be echoed in the reply.
        qos: RequestQoSType,
        /// Optional request body.
        body: Option<ZBuf>,
        /// Optional request attachment.
        attachment: Option<ZBuf>,
    },
    /// A successful reply chunk for a get.
    Reply(ZBuf),
    /// An error reply chunk for a get.
    ReplyErr(ZBuf),
    /// The final response for a get, signalling no more replies.
    Final,
}

/// Type-erased handle to a [`SubscriberPool`].
///
/// Hides the pool's mutex type and const-generic sizes so that [`Session`](crate::session::Session)
/// and consumers carry no extra generics.
pub trait Dispatch {
    /// Claim the first free slot for `route`, returning its index, or `None` if the pool is full.
    fn register(&self, route: Route) -> Option<usize>;

    /// Release the slot at `idx` and drop any buffered messages still in its channel.
    fn unregister(&self, idx: usize);

    /// Obtain the receiver for the private channel of slot `idx`.
    fn receiver(&self, idx: usize) -> DynamicReceiver<'_, Routed>;

    /// Route a received message to the matching slot(s), dropping (with a warning) on a full
    /// channel so that a slow consumer only loses its own messages.
    fn deliver(&self, msg: &NetworkMessage);

    /// Append a re-declare message for every registered subscriber/queryable to `out`.
    fn collect_redeclares(&self, out: &mut Vec<NetworkMessage>);
}

/// A statically sized pool of consumer slots backing the [`Dispatch`] routing.
///
/// `SLOTS` is the number of concurrent consumers (subscribers + queryables + in-flight gets);
/// `SLOT_DEPTH` is how many messages each consumer's private channel buffers before the dispatcher
/// starts dropping for that consumer.
pub struct SubscriberPool<
    M: RawMutex = NoopRawMutex,
    const SLOTS: usize = DEFAULT_SLOTS,
    const SLOT_DEPTH: usize = DEFAULT_SLOT_DEPTH,
> {
    channels: [Channel<M, Routed, SLOT_DEPTH>; SLOTS],
    routes: BlockingMutex<M, RefCell<[Option<Route>; SLOTS]>>,
}

impl<M: RawMutex, const SLOTS: usize, const SLOT_DEPTH: usize>
    SubscriberPool<M, SLOTS, SLOT_DEPTH>
{
    /// Create a new, empty pool.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            channels: [const { Channel::new() }; SLOTS],
            routes: BlockingMutex::new(RefCell::new([const { None }; SLOTS])),
        }
    }

    /// Try to send `routed` to slot `idx`, warning (and dropping) if the channel is full.
    ///
    /// NOTE: this (and `deliver`) run while the `routes` lock is held, and they allocate
    /// (`String`/`ZBuf` clones) and log. That is fine for the default single-executor
    /// `NoopRawMutex`, but instantiating [`SubscriberPool`] with an interrupt-masking mutex (e.g.
    /// `CriticalSectionRawMutex`) would allocate and log with interrupts disabled — avoid that.
    fn try_route(&self, idx: usize, routed: Routed) {
        if self.channels[idx].try_send(routed).is_err() {
            warn!("Dropping message for consumer slot {}: channel full", idx);
        }
    }
}

impl<M: RawMutex, const SLOTS: usize, const SLOT_DEPTH: usize> Default
    for SubscriberPool<M, SLOTS, SLOT_DEPTH>
{
    fn default() -> Self {
        Self::new()
    }
}

impl<M: RawMutex, const SLOTS: usize, const SLOT_DEPTH: usize> Dispatch
    for SubscriberPool<M, SLOTS, SLOT_DEPTH>
{
    fn register(&self, route: Route) -> Option<usize> {
        self.routes.lock(|routes| {
            let mut routes = routes.borrow_mut();
            let idx = routes.iter().position(Option::is_none)?;
            routes[idx] = Some(route);
            Some(idx)
        })
    }

    fn unregister(&self, idx: usize) {
        self.routes.lock(|routes| {
            routes.borrow_mut()[idx] = None;
            // Clear the channel while still holding the routing lock. `register`, `deliver` and
            // `unregister` all take this lock and never await, so this makes slot teardown atomic
            // w.r.t. them: a slot can never be re-registered as a different kind while a stale
            // message from the previous owner is still buffered. This is what lets the consumers
            // treat an unexpected `Routed` variant as an unreachable internal bug.
            self.channels[idx].clear();
        });
    }

    fn receiver(&self, idx: usize) -> DynamicReceiver<'_, Routed> {
        self.channels[idx].dyn_receiver()
    }

    fn deliver(&self, msg: &NetworkMessage) {
        match &msg.body {
            NetworkBody::Push(Push {
                wire_expr,
                payload: PushBody::Put(Put { payload, .. }),
                ..
            }) => {
                let key = wire_expr.suffix.as_ref();
                self.routes.lock(|routes| {
                    for (idx, slot) in routes.borrow().iter().enumerate() {
                        if let Some(Route::Subscriber { key: declared, .. }) = slot
                            && key_matches(declared, key)
                        {
                            self.try_route(
                                idx,
                                Routed::Push {
                                    key: String::from(key),
                                    payload: payload.clone(),
                                },
                            );
                        }
                    }
                });
            }
            NetworkBody::Request(Request {
                id,
                wire_expr,
                ext_qos,
                payload: RequestBody::Query(query),
                ..
            }) => {
                let key = wire_expr.as_str();
                self.routes.lock(|routes| {
                    for (idx, slot) in routes.borrow().iter().enumerate() {
                        if let Some(Route::Queryable { key: declared, .. }) = slot
                            && key_matches(declared, key)
                        {
                            self.try_route(
                                idx,
                                Routed::Query {
                                    request_id: *id,
                                    qos: *ext_qos,
                                    body: query.ext_body.as_ref().map(|b| b.payload.clone()),
                                    attachment: query
                                        .ext_attachment
                                        .as_ref()
                                        .map(|a| a.buffer.clone()),
                                },
                            );
                        }
                    }
                });
            }
            NetworkBody::Response(Response { rid, payload, .. }) => {
                let routed = match payload {
                    ResponseBody::Reply(Reply {
                        payload: ReplyBody::Put(Put { payload, .. }),
                        ..
                    }) => Routed::Reply(payload.clone()),
                    ResponseBody::Err(ZErr { payload, .. }) => Routed::ReplyErr(payload.clone()),
                    _ => return,
                };
                self.route_response(*rid, routed);
            }
            NetworkBody::ResponseFinal(ResponseFinal { rid, .. }) => {
                self.route_response(*rid, Routed::Final);
            }
            _ => {}
        }
    }

    fn collect_redeclares(&self, out: &mut Vec<NetworkMessage>) {
        self.routes.lock(|routes| {
            for slot in routes.borrow().iter().flatten() {
                match slot {
                    Route::Subscriber { redeclare, .. } | Route::Queryable { redeclare, .. } => {
                        out.push(redeclare.clone());
                    }
                    Route::Get { .. } => {}
                }
            }
        });
    }
}

impl<M: RawMutex, const SLOTS: usize, const SLOT_DEPTH: usize>
    SubscriberPool<M, SLOTS, SLOT_DEPTH>
{
    /// Route a get response to the unique slot matching `request_id`.
    fn route_response(&self, request_id: RequestId, routed: Routed) {
        // Only one slot can match a given request id, so build `routed` once and hand it over.
        let mut routed = Some(routed);
        self.routes.lock(|routes| {
            for (idx, slot) in routes.borrow().iter().enumerate() {
                if let Some(Route::Get { request_id: rid }) = slot
                    && *rid == request_id
                {
                    if let Some(routed) = routed.take() {
                        self.try_route(idx, routed);
                    }
                    break;
                }
            }
        });
    }
}
