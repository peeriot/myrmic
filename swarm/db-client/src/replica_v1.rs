use db_commons::models::replication;
use db_commons::models::{ReplicaMessage, Scope, Subject, Version, locate};
use zenoh::Session;
use zenoh_result::ZResult;

/// The window for one sync request: a pull page or a verify check. Off the
/// hot path, and a page can take whole seconds on a slow link — generous.
const SYNC_TIMEOUT: core::time::Duration = core::time::Duration::from_secs(15);

/// A short window for [`Client::locate`]. Discovery sits on the critical path of a routed
/// `tx_begin`, so it must resolve quickly — but "quickly" has to leave room for a real
/// query/reply round trip, not just same-machine loopback: 50ms (this constant's previous value)
/// reliably starved a caller reaching the mesh through an SSH tunnel (e.g. a benchmark driver) or
/// even LAN peers under load, causing `locate` to spuriously find no candidates and silently fall
/// back to `super::v1::impls::any_node`'s unrelated-to-the-real-holder "highest zenoh ID" pick
/// — observed directly: the exact same query, from the exact same session, returning real stored
/// data on one call and nothing at all moments later. Matches the `nano`/embedded discovery path's
/// own already-tuned `super::v1::impls::LOCATE_TIMEOUT` (500ms), which never had this problem.
const LOCATE_TIMEOUT: core::time::Duration = core::time::Duration::from_millis(500);

/// A replicating node's answer to a [`Client::locate`] query: its own head for
/// the scope plus the live peers it vouches for, so one reply surfaces the
/// whole set.
pub type Located = locate::Response;

/// This client can be used for publishing and subscribing to replica messages.
///
/// Typically, this is internal use, but it could be used to add an external storage mechanism.
#[derive(Debug, Clone)]
pub struct Client {
    session: Session,
    broadcast: String,
    subject: Subject,
}

impl Client {
    pub fn new(session: &Session, subject: Subject) -> ZResult<Self> {
        let me = session.zid();

        let (namespace, db, schema) = subject.as_keyexprs();

        let broadcast = db_commons::topics::replica::format_replica(namespace, db, schema, me, "*");

        Ok(Self {
            session: session.clone(),
            broadcast,
            subject,
        })
    }

    /// Broadcasts a `ReplicaMessage` to other clients using the same key.
    pub async fn publish(&self, msg: ReplicaMessage) {
        tracing::debug!("[{}] sending {}", self.session.zid(), msg.name());

        let msg = postcard::to_allocvec(&msg).expect("unable to ser msg");

        self.session
            .put(&self.broadcast, msg)
            .await
            .expect("unable to publish replica message");
    }

    /// Asks replicating nodes which of them hold `scope` at at least
    /// `min_version` (any version when `None`).
    ///
    /// Zenoh routes the query only to nodes replicating a subject that covers
    /// `scope`; each such node replies only if it qualifies, so the result is
    /// the set of nodes that answered within `LOCATE_TIMEOUT`.
    pub async fn locate(
        &self,
        scope: &Scope,
        min_version: Option<Version>,
    ) -> ZResult<Vec<Located>> {
        use futures::StreamExt;

        Ok(self
            .locate_stream(scope, min_version)
            .await?
            .collect::<Vec<_>>()
            .await)
    }

    /// [`Self::locate`], but yielding each reply as it arrives instead of
    /// draining the query first. A `QueryTarget::All` locate only finishes
    /// once every matching queryable has finalised (or `LOCATE_TIMEOUT`
    /// expires), so a collect-everything caller always pays for the slowest
    /// replicating node — this lets a caller that can already act on an early
    /// reply (see `v1::impls::locate_holder`) stop consuming, and with it stop
    /// waiting, as soon as it has what it needs.
    pub async fn locate_stream(
        &self,
        scope: &Scope,
        min_version: Option<Version>,
    ) -> ZResult<impl futures::Stream<Item = Located>> {
        use futures::StreamExt;
        use zenoh::qos::Priority;
        use zenoh::query::{ConsolidationMode, QueryTarget};

        let ke = db_commons::topics::replica_query::format(
            &scope.namespace,
            &scope.database,
            &scope.schema,
        );
        let data = postcard::to_allocvec(&locate::Request { min_version })
            .expect("unable to serialise locate request");

        // Discovery must outrank the data plane on the wire: locate queries
        // (and their replies, which inherit the query's priority) otherwise
        // share default-priority queues with the very request flood they are
        // trying to route around, and a drowned round is indistinguishable
        // from "no holder" (run 33257380391: ~8,000 empty locates in one
        // 25 s pass). Express skips batching for the same reason — a locate
        // is small and latency-bound, never throughput-bound.
        Ok(self
            .session
            .get(ke)
            .payload(data)
            .target(QueryTarget::All)
            .consolidation(ConsolidationMode::None)
            .priority(Priority::InteractiveHigh)
            .express(true)
            .timeout(LOCATE_TIMEOUT)
            .await?
            .into_stream()
            .filter_map(|reply| async move {
                let sample = reply.into_result().ok()?;
                db_commons::query::parse_sample::<locate::Response>(&sample)
            }))
    }

    /// Sends one sync request (a pull page or a coverage check) to `target`'s
    /// sync queryable for `scope`, returning the first reply. `None` on any
    /// failure — callers fall back to broadcast gossip.
    async fn sync_request(
        &self,
        target: uhlc::ID,
        scope: &Scope,
        req: &replication::sync::Request,
    ) -> Option<replication::sync::Response> {
        let ke = db_commons::topics::replica_sync::format(
            target,
            &scope.namespace,
            &scope.database,
            &scope.schema,
        );
        let data = postcard::to_allocvec(req).ok()?;

        let replies = self
            .session
            .get(ke)
            .payload(data)
            .timeout(SYNC_TIMEOUT)
            .await
            .ok()?;
        let reply = replies.recv_async().await.ok()?;
        let sample = reply.into_result().ok()?;

        postcard::from_bytes(&sample.payload().to_bytes()).ok()
    }

    /// One page of a direct catch-up pull from `target`.
    pub async fn sync_pull(
        &self,
        target: uhlc::ID,
        req: replication::sync::PullRequest,
    ) -> Option<replication::sync::PullResponse> {
        let scope = req.scope.clone();
        match self
            .sync_request(target, &scope, &replication::sync::Request::Pull(req))
            .await?
        {
            replication::sync::Response::Pull(resp) => Some(resp),
            replication::sync::Response::Verify(_) => None,
        }
    }

    /// Asks `target` whether it covers a page of heads.
    pub async fn sync_verify(
        &self,
        target: uhlc::ID,
        req: replication::sync::VerifyRequest,
    ) -> Option<bool> {
        let scope = req.scope.clone();
        match self
            .sync_request(target, &scope, &replication::sync::Request::Verify(req))
            .await?
        {
            replication::sync::Response::Verify(resp) => Some(resp.covered),
            replication::sync::Response::Pull(_) => None,
        }
    }

    /// Subscribes to messages addressed to clients with the same key.
    ///
    /// The provided closure is called for each incoming message.
    pub async fn subscribe<F>(&self, func: F) -> zenoh::pubsub::Subscriber<()>
    where
        F: Fn(uhlc::ID, ReplicaMessage) + Send + Sync + 'static,
    {
        let me: uhlc::ID = self.session.zid().into();

        let (namespace, db, schema) = self.subject.as_keyexprs();

        let sub_ke = db_commons::topics::replica::format_replica(namespace, db, schema, "*", me);

        self.session
            .declare_subscriber(sub_ke)
            .callback({
                let func = func;

                move |sample| {
                    let func = &func;

                    if let Some((id, msg)) = handle_query(me, &sample) {
                        func(id, msg);
                    }
                }
            })
            .await
            .expect("unable to register subscriber")
    }
}

fn handle_query(
    me: uhlc::ID,
    sample: &zenoh::sample::Sample,
) -> Option<(uhlc::ID, ReplicaMessage)> {
    let sender = match db_commons::topics::replica::parse_sender(sample.key_expr()) {
        Ok(id) => id,
        Err(err) => {
            tracing::error!(
                "[{}] Unable to parse key_expr [{}]: {}",
                me,
                sample.key_expr(),
                err
            );
            return None;
        }
    };

    if me == sender {
        return None;
    }

    let Some(req) = db_commons::query::parse_sample::<ReplicaMessage>(sample) else {
        tracing::error!(
            "[{}] Unable to parse incoming query from {}. [dropped {}]",
            me,
            sender,
            sample.key_expr()
        );
        return None;
    };

    Some((sender, req))
}
