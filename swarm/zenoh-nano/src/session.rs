//! Session

use core::cell::Cell;
use core::pin::pin;

use alloc::vec::Vec;

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex;
use embassy_sync::blocking_mutex::raw::{NoopRawMutex, RawMutex};
use embassy_sync::channel::DynamicReceiver;
use embassy_sync::mutex::Mutex;
use embassy_sync::pubsub::{DynPublisher, DynSubscriber, PubSubChannel};
use embassy_time::{Duration, TimeoutError, Timer, with_timeout};

use portable_atomic::{AtomicU16, AtomicU32, Ordering};

use zenoh_protocol::core::{ExprId, Reliability, WireExpr, ZenohIdProto};
use zenoh_protocol::network::declare::ext::NodeIdType;
use zenoh_protocol::network::declare::{QueryableId, SubscriberId, TokenId};
use zenoh_protocol::network::ext::QoSType as NQoSType;
use zenoh_protocol::network::{Declare, DeclareBody, DeclareToken, NetworkBody, RequestId};
use zenoh_protocol::transport::{KeepAlive, TransportBody, TransportMessage};

use crate::clock::{Clock, message_timestamp};
use crate::dispatch::{Dispatch, Route, Routed};
use crate::link::{LinkReceive, LinkSend};
use crate::network::{
    IncomingMessage, Network, NetworkError, NetworkReceive, NetworkSend, OutgoingMessage,
};
use crate::ops::publish::MAX_PUBLISHERS;
use crate::transport::SessionConfig;

/// Pool of PubSub Publishers
/// Cater for zenoh publishers plus some for queryables and the dispatcher's re-declare publisher
const PUB_CAPACITY: usize = MAX_PUBLISHERS + 10;

/// Session error
#[derive(thiserror::Error, Debug, Copy, Clone, Eq, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum SessionError {
    /// Publisher capacity needs to be increased
    #[error("publisher capacity needs to be increased")]
    NoPublishCapacity,
    /// Dispatcher capacity needs to be increased
    #[error("dispatcher capacity needs to be increased")]
    NoDispatcherCapacity,
    /// Network error
    #[error("network error: {0}")]
    Network(#[from] NetworkError),
}

/// Resources associated with a session
///
/// This is separate from the session itself so that the resources can be allocated in a const context
pub struct SessionResources<M: RawMutex = NoopRawMutex> {
    /// Outgoing messages to the network
    outgoing: PubSubChannel<M, OutgoingMessage, 5, 1, PUB_CAPACITY>,
    /// Our ZID
    /// Only valid when a session is already established
    zid: blocking_mutex::Mutex<M, Cell<Option<ZenohIdProto>>>,
    /// Peer ZID
    /// Only valid when a session is already established
    peer_zid: blocking_mutex::Mutex<M, Cell<Option<ZenohIdProto>>>,
    /// Sequence Number for the session
    ///
    /// Initially 0, and only valid once a session is already established
    sn: AtomicU32,
    /// Request ID tracker
    ///
    /// Used to obtain unique request ids for get operations
    rid: AtomicU32,
    /// Expression ID Tracker
    ///
    /// Used to obtain unique expression ids for declare operations
    eid: AtomicU16,
    /// Subscriber ID Tracker
    ///
    /// Used to obtain unique subscriber ids
    sid: AtomicU32,
    /// Queryable ID Tracker
    ///
    /// Used to obtain unique queryable ids so multiple queryables on one session don't collide
    qid: AtomicU32,
    /// Key-expression prefix for the liveliness token, or `None` if liveliness is disabled.
    ///
    /// The full token key is `{prefix}{our_zid}`. See [`Session::enable_liveliness`].
    liveliness_prefix: blocking_mutex::Mutex<M, Cell<Option<&'static str>>>,
    /// Connection epoch: incremented once per established transport.
    ///
    /// 0 until the first connection. See [`Session::connection_epoch`].
    connection_epoch: AtomicU32,
    /// Application-owned clock, or `None` if unset. See [`Session::set_clock`].
    clock: blocking_mutex::Mutex<M, Cell<Option<&'static dyn Clock>>>,
}

/// Id of the liveliness token. There is at most one token per session, so a constant suffices.
const LIVELINESS_TOKEN_ID: TokenId = 0;

impl<M: RawMutex> SessionResources<M> {
    /// Create new session resources
    #[must_use]
    pub const fn new() -> Self {
        Self {
            outgoing: PubSubChannel::new(),
            zid: blocking_mutex::Mutex::new(Cell::new(None)),
            peer_zid: blocking_mutex::Mutex::new(Cell::new(None)),
            sn: AtomicU32::new(0),
            rid: AtomicU32::new(0),
            eid: AtomicU16::new(0),
            sid: AtomicU32::new(0),
            qid: AtomicU32::new(0),
            liveliness_prefix: blocking_mutex::Mutex::new(Cell::new(None)),
            connection_epoch: AtomicU32::new(0),
            clock: blocking_mutex::Mutex::new(Cell::new(None)),
        }
    }

    /// Reset the session resources
    fn reset(&self) {
        self.outgoing.clear();
        self.zid.lock(|zid| zid.set(None));
        self.peer_zid.lock(|zid| zid.set(None));
        self.sn.store(0, Ordering::Relaxed);
        self.rid.store(0, Ordering::Relaxed);
        self.eid.store(0, Ordering::Relaxed);
        self.sid.store(0, Ordering::Relaxed);
        self.qid.store(0, Ordering::Relaxed);
        self.liveliness_prefix.lock(|p| p.set(None));
        self.connection_epoch.store(0, Ordering::Relaxed);
        self.clock.lock(|c| c.set(None));
    }
}

impl<M: RawMutex> Default for SessionResources<M> {
    fn default() -> Self {
        Self::new()
    }
}

/// A Zenoh session
pub struct Session<'a, M: RawMutex = NoopRawMutex> {
    resources: &'a SessionResources<M>,
    dispatch: &'a dyn Dispatch,
}

impl<'a, M: RawMutex> core::fmt::Debug for Session<'a, M> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Session(<zid>)")?;
        Ok(())
    }
}

impl<'a, M: RawMutex> Session<'a, M> {
    /// Create a new session from the given resources
    ///
    /// # Arguments
    /// - `resources`: The session resources to use
    /// - `pool`: The consumer-slot pool backing the message dispatcher
    ///
    /// # Returns
    /// A tuple of the session and a session runner
    #[must_use]
    pub fn new(
        resources: &'a SessionResources<M>,
        subscriber_pool: &'a dyn Dispatch,
    ) -> (Self, SessionRunner<'a, M>) {
        resources.reset();

        (
            Self {
                resources,
                dispatch: subscriber_pool,
            },
            SessionRunner::new(resources, subscriber_pool),
        )
    }

    /// Return a publisher for outgoing messages
    ///
    /// # Errors
    ///
    /// If there's no more capacity in the messages' outgoing queue
    pub(crate) fn publisher(&self) -> Result<DynPublisher<'a, OutgoingMessage>, SessionError> {
        self.resources
            .outgoing
            .dyn_publisher()
            .map_err(|_| SessionError::NoPublishCapacity)
    }

    /// Claim a dispatcher slot for `route`, returning its index and private-channel receiver.
    ///
    /// # Errors
    ///
    /// If there are no more free slots in the dispatcher pool
    pub(crate) fn register(
        &self,
        route: Route,
    ) -> Result<(usize, DynamicReceiver<'a, Routed>), SessionError> {
        let dispatch = self.dispatch;
        let slot = dispatch
            .register(route)
            .ok_or(SessionError::NoDispatcherCapacity)?;
        Ok((slot, dispatch.receiver(slot)))
    }

    /// The type-erased dispatcher handle, so consumers can release their slot on drop.
    pub(crate) fn dispatch(&self) -> &'a dyn Dispatch {
        self.dispatch
    }

    /// Return our ZID
    ///
    /// This method will wait until the ZID is set, i.e. when the session runner has started
    pub async fn zid(&self) -> ZenohIdProto {
        loop {
            if let Some(zid) = self.resources.zid.lock(|zid| zid.get()) {
                break zid;
            }

            // Wait a bit and try again
            // Not ideal but this should be set very early in the session
            Timer::after(Duration::from_millis(50)).await;
        }
    }

    /// Return the peer ZID
    ///
    /// This method will wait until the peer ZID is set, i.e. when the session runner has started
    // TODO: Left as unused for now, but will be useful for future features
    #[allow(unused)]
    pub(crate) async fn peer_zid(&self) -> ZenohIdProto {
        loop {
            if let Some(zid) = self.resources.peer_zid.lock(|zid| zid.get()) {
                break zid;
            }

            // Wait a bit and try again
            // Not ideal but this should be set very early in the session
            Timer::after(Duration::from_millis(50)).await;
        }
    }

    /// Obtain a new request id
    pub(crate) fn get_new_rid(&self) -> RequestId {
        self.resources.rid.fetch_add(1, Ordering::Relaxed)
    }

    /// Obtain a new expression id
    pub(crate) fn get_new_eid(&self) -> ExprId {
        self.resources.eid.fetch_add(1, Ordering::Relaxed)
    }

    /// Obtain a new subscriber id
    pub(crate) fn get_new_sid(&self) -> SubscriberId {
        self.resources.sid.fetch_add(1, Ordering::Relaxed)
    }

    /// Obtain a new queryable id
    pub(crate) fn get_new_qid(&self) -> QueryableId {
        self.resources.qid.fetch_add(1, Ordering::Relaxed)
    }

    /// The connection epoch: incremented each time a transport (re)connects,
    /// 0 until the first connection. Consumers can poll it to detect
    /// reconnects and refresh their network-visible state (re-announce
    /// themselves) promptly instead of waiting for their next periodic round.
    #[must_use]
    pub fn connection_epoch(&self) -> u32 {
        self.resources.connection_epoch.load(Ordering::Relaxed)
    }

    /// Enable liveliness for this session.
    ///
    /// Once enabled, on every (re)connection the runner declares a liveliness token on
    /// `{ke_prefix}{our_zid}`, letting other nodes detect when this device drops off the network
    /// (the token is implicitly undeclared when the transport session dies). Call this before
    /// starting the runner; otherwise it takes effect on the next (re)connection.
    pub fn enable_liveliness(&self, ke_prefix: &'static str) {
        self.resources
            .liveliness_prefix
            .lock(|p| p.set(Some(ke_prefix)));
    }

    /// Register an application-owned clock for this session.
    ///
    /// The session runner feeds it the timestamp of every received message,
    /// and outgoing puts and replies are stamped with [`Clock::timestamp`].
    /// Call this before declaring publishers: they capture the clock at
    /// declare time.
    pub fn set_clock(&self, clock: &'static dyn Clock) {
        self.resources.clock.lock(|c| c.set(Some(clock)));
    }

    /// The registered clock, if any.
    pub(crate) fn clock(&self) -> Option<&'static dyn Clock> {
        self.resources.clock.lock(|c| c.get())
    }
}

impl<'a, M: RawMutex> Clone for Session<'a, M> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<'a, M: RawMutex> Copy for Session<'a, M> {}

/// Build the `DeclareToken` network message declaring this session's liveliness token,
/// keyed on `{ke_prefix}{zid}`.
fn liveliness_declare(zid: ZenohIdProto, ke_prefix: &str) -> OutgoingMessage {
    let key = alloc::format!("{ke_prefix}{zid}");

    OutgoingMessage {
        body: NetworkBody::Declare(Declare {
            interest_id: None,
            ext_qos: NQoSType::DECLARE,
            ext_tstamp: None,
            ext_nodeid: NodeIdType::DEFAULT,
            body: DeclareBody::DeclareToken(DeclareToken {
                id: LIVELINESS_TOKEN_ID,
                wire_expr: WireExpr::empty().with_suffix(key.as_str()).to_owned(),
            }),
        }),
        reliability: Reliability::Reliable,
    }
}

/// Session runner
pub struct SessionRunner<'a, M: RawMutex = NoopRawMutex> {
    resources: &'a SessionResources<M>,
    dispatch: &'a dyn Dispatch,
    pub_sub: Mutex<
        M,
        (
            DynPublisher<'a, OutgoingMessage>,
            DynSubscriber<'a, OutgoingMessage>,
        ),
    >,
}

impl<'a, M: RawMutex> SessionRunner<'a, M> {
    /// Create a new session runner from the given resources
    #[must_use]
    fn new(resources: &'a SessionResources<M>, dispatch: &'a dyn Dispatch) -> Self {
        Self {
            resources,
            dispatch,
            // The first publisher is used by the dispatcher to re-declare subscriptions after a
            // reconnect; the subscriber drives the outgoing send loop.
            pub_sub: Mutex::new((
                unwrap!(resources.outgoing.dyn_publisher()),
                unwrap!(resources.outgoing.dyn_subscriber()),
            )),
        }
    }

    /// Run the session
    ///
    /// This function will run indefinitely until an error occurs
    ///
    /// # Arguments
    /// - `network`: The network to use for the session
    ///
    /// # Errors
    /// - If a network error occurs
    pub async fn run<R: LinkReceive, S: LinkSend>(
        &mut self,
        mut network: Network<'_, R, S>,
    ) -> Result<(), SessionError> {
        self.resources
            .zid
            .lock(|zid| zid.set(Some(network.config.our_zid)));
        self.resources
            .peer_zid
            .lock(|zid| zid.set(Some(network.config.peer_zid)));

        self.resources
            .sn
            .store(network.config.initial_sn, Ordering::Relaxed);
        self.resources
            .connection_epoch
            .fetch_add(1, Ordering::Relaxed);

        let mut pub_sub = self.pub_sub.lock().await;
        let pub_sub = &mut *pub_sub;

        let redeclare_publisher = &mut pub_sub.0;
        let subscriber = &mut pub_sub.1;

        // A new session transport is up. Re-declare every registered subscription/queryable to the
        // new router, which otherwise has no knowledge of them. On the first connect this simply
        // re-affirms the declares the consumers already sent; after a reconnect it is what restores
        // them.
        let mut redeclares = Vec::new();
        self.dispatch.collect_redeclares(&mut redeclares);

        // If liveliness is enabled, declare a liveliness token so other nodes can detect when
        // this device drops off the network. Sent on every (re)connection because the token is
        // tied to the transport session and is implicitly undeclared when that session dies.
        if let Some(prefix) = self.resources.liveliness_prefix.lock(|p| p.get()) {
            let msg = liveliness_declare(network.config.our_zid, prefix);
            network
                .send
                .send(&network.config, &self.resources.sn, msg)
                .await?;
        }

        // Publish the re-declares as the first thing the incoming task does, so they run
        // *concurrently* with the outgoing drain loop below. Doing it before entering the select
        // would deadlock: the re-declares can exceed the outgoing channel capacity, and nothing is
        // draining that channel until `outgoing` is being polled.
        let incoming = async {
            for msg in redeclares {
                redeclare_publisher.publish(msg).await;
            }
            self.incoming(&mut network.receive, &network.config).await
        };
        let mut incoming = pin!(incoming);
        let mut outgoing = pin!(self.outgoing(&mut network.send, &network.config, subscriber));

        let result = select(&mut incoming, &mut outgoing).await;

        match result {
            Either::First(res) => res,
            Either::Second(res) => res,
        }
    }

    /// Handle incoming messages
    ///
    /// This function will run indefinitely until an error occurs
    ///
    /// # Arguments
    /// - `receive`: The network receiver to use
    /// - `config`: The session configuration
    async fn incoming<R: LinkReceive>(
        &self,
        receive: &mut NetworkReceive<R>,
        config: &SessionConfig,
    ) -> Result<(), SessionError> {
        loop {
            let msg = receive.receive(config).await?;
            trace!("Receiving {:?}", msg);

            match msg {
                IncomingMessage::Message(msg) => {
                    if let Some(clock) = self.resources.clock.lock(|c| c.get())
                        && let Some(ts) = message_timestamp(&msg)
                    {
                        clock.observe(ts);
                    }
                    self.dispatch.deliver(&msg);
                }
                // `Close` is not routed to consumers; a genuine peer close will
                // surface as a transport error on the next receive and end the session.
                IncomingMessage::Close(_) => {}
            }
        }
    }

    /// Handle outgoing messages
    ///
    /// This function will run indefinitely until an error occurs
    ///
    /// # Arguments
    /// - `send`: The network sender to use
    /// - `config`: The session configuration
    /// - `subscriber`: The subscriber to use for outgoing messages
    async fn outgoing<S: LinkSend>(
        &self,
        send: &mut NetworkSend<S>,
        config: &SessionConfig,
        subscriber: &mut DynSubscriber<'a, OutgoingMessage>,
    ) -> Result<(), SessionError> {
        let lease = config.lease;

        // Make sure to respond to Keep Alive messages or the session is going to get dropped by the
        // router.
        //  Reference from zenoh-protocol:
        //      NOTE: In order to consider eventual packet loss, transmission latency and jitter, the time
        //      interval between two subsequent [`KeepAlive`] messages SHOULD be set to one fourth of
        //      the lease time. This is in-line with the ITU-T G.8013/Y.1731 specification on continuous
        //      connectivity check which considers a link as failed when no messages are received in
        //      3.5 times the target keep alive interval.
        let timeout = lease / 4;

        loop {
            let result = with_timeout(timeout, subscriber.next_message_pure()).await;

            match result {
                Ok(msg) => {
                    trace!("Sending {:?}", msg);
                    send.send(config, &self.resources.sn, msg).await?;
                }
                Err(TimeoutError) => {
                    send.0
                        .send(
                            &TransportMessage {
                                body: TransportBody::KeepAlive(KeepAlive {}),
                            },
                            None,
                        )
                        .await
                        .map_err(NetworkError::Transport)?;
                }
            }
        }
    }
}
