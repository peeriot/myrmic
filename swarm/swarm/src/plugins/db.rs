use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::sync::Notify;
use zenoh::{Result as ZResult, Session};

use config::{Config, StoreConfig};

use crate::plugins::MyrmicCtx;
use db::store::TransactionOptions;
use db::store::fjall::RemoteTx;
use db_commons::models;

mod apply;
mod config;
mod handler;
mod load_from;
mod metrics;
mod replica_sets;
mod replication;

#[cfg(test)]
mod tests;

pub struct Plugin;

impl crate::plugins::MyrmicPlugin for Plugin {
    const DEFAULT_NAME: &'static str = "db";

    type Config = Config;

    async fn main(ctx: MyrmicCtx, config: Self::Config) -> ZResult<()> {
        let Config {
            store: store_config,
            load_from,
            tags: _,
        } = config;

        let session = ctx.session().clone();
        let handle = ctx.handle().clone();

        let Some(hlc) = session.hlc() else {
            panic!("unable to start db [HLC not enabled]");
        };

        let tx_idle_timeout = store_config
            .tx_idle_timeout
            .unwrap_or(DEFAULT_TX_IDLE_TIMEOUT);

        let escalation_timeout = store_config
            .offload_escalation_timeout
            .unwrap_or(DEFAULT_OFFLOAD_ESCALATION_TIMEOUT);

        let mut store = build_store(store_config, hlc);

        let token = store.shutdown_token();

        load_from::load_from(&mut store, load_from)?;

        let context = StoreContext::new(
            session.clone(),
            handle.clone(),
            store,
            tx_idle_timeout,
            escalation_timeout,
        );

        for subject in replica_sets::unconditional() {
            context.start_replication(subject).await;
        }

        handle.spawn(replica_sets::run(
            context.clone(),
            db_client::v1::Client::new(&session),
            ctx.tags().clone(),
        ));

        // Backstop for stray scopes the commit-time hook can't see: data held
        // from before a configuration change or an earlier run. Sleep-first,
        // so the replication watcher settles before anything is offered up.
        handle.spawn({
            let context = context.clone();
            let shutdown = context.store.shutdown_token();

            async move {
                loop {
                    tokio::select! {
                        () = tokio::time::sleep(STRAY_SCAN_INTERVAL) => (),
                        () = shutdown.cancelled() => break,
                    }

                    match context.store.stray_scopes() {
                        Ok(scopes) => {
                            for scope in scopes {
                                context.start_offload(scope, OffloadKind::Hidden);
                            }
                        }
                        Err(err) => tracing::warn!("unable to scan for stray scopes: {err}"),
                    }
                }
            }
        });

        let query = db_commons::topics::format_query(session.zid());

        let queryable = session
            .declare_queryable(query)
            .callback({
                let handle = handle.clone();
                let context = context.clone();

                move |query| {
                    let fut = handler::handle_query(context.clone(), query);

                    handle.spawn(fut);
                }
            })
            .await
            .expect("Unable to setup queryable");

        tokio::spawn({
            let drop_rx = ctx.drop_notifier();

            async move {
                let _drop = drop_rx.recv_async().await.ok();

                if let Err(err) = queryable.undeclare().await {
                    tracing::error!("unable to stop db handler: {}", err);
                }
                token.cancel();
            }
        });

        ctx.notify_ready();

        Ok(())
    }
}

fn build_store(config: StoreConfig, hlc: Arc<uhlc::HLC>) -> db::store::fjall::Store<TxEvents> {
    let StoreConfig {
        directory,
        gc_interval,
        // Applied per-transaction when one is begun, not a store-level option.
        tx_idle_timeout: _,
        // A StoreContext concern, read before the store is built.
        offload_escalation_timeout: _,
    } = config;

    let opts = db::store::Options {
        directory,
        logic_clock: hlc,
        gc_interval,
    };

    db::store::fjall::Store::init(opts).unwrap()
}

/// Tables an open transaction has inserted into, carried as the
/// transaction's metadata. Published as events once it durably commits.
type TxEvents = HashSet<(models::Scope, models::Table)>;

const DEFAULT_TX_IDLE_TIMEOUT: Duration = Duration::from_mins(5);

/// Cadence of the background sweep for scopes to offload.
const STRAY_SCAN_INTERVAL: Duration = Duration::from_mins(1);

/// How long an offloader serves a scope no replica has taken over before it
/// escalates itself into a durable replica. Long enough that a transiently
/// unreachable replica (GC pause, WiFi blip) can recover and cover it first.
const DEFAULT_OFFLOAD_ESCALATION_TIMEOUT: Duration = Duration::from_secs(30);

/// How a [`StoreContext::start_offload`] drain takes part in routing, and
/// when it may escalate itself back into a replica.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffloadKind {
    /// Shedding a scope this node stopped replicating (or found stranded).
    /// Hidden from locate — it is not trying to attract transactions — but
    /// escalation is armed: uncovered data must still find a durable home.
    Hidden,
    /// A fallback write landed here because no replica was locatable: the
    /// scope stays findable so subsequent writes consolidate onto this node,
    /// and escalation is armed.
    Sink,
    /// A demoted provisional deferring to a better holder. Findable (it may
    /// be the only holder of its undrained rows) but the escalation timer is
    /// *not* armed — that would ping-pong with the winner — and re-arms only
    /// on evidence the target is unreachable: a fresh fallback write landing
    /// here, or every full replica for the scope going quiet.
    Unwinding {
        /// The holder this drain defers to; diagnostics only.
        target: models::NodeId,
    },
}

#[derive(Clone)]
struct StoreContext {
    handle: Handle,
    session: Session,
    store: db::store::fjall::Store<TxEvents>,
    tx_idle_timeout: Duration,
    escalation_timeout: Duration,
    /// Escalation re-arm signals of the running offloaders, keyed by scope.
    offload_rearms: Arc<Mutex<HashMap<models::Scope, Arc<Notify>>>>,
    /// Per-scope "a commit just stranded data here" signal, woken by
    /// [`finish_commit`](Self::finish_commit). Separate from
    /// `offload_rearms` on purpose: a commit is not evidence that a drain's
    /// deference target is unreachable, and conflating the two would re-arm
    /// escalation on every write and flap the drain.
    offload_nudges: Arc<Mutex<HashMap<models::Scope, Arc<Notify>>>>,
}

impl StoreContext {
    pub fn new(
        session: Session,
        handle: Handle,
        store: db::store::fjall::Store<TxEvents>,
        tx_idle_timeout: Duration,
        escalation_timeout: Duration,
    ) -> Self {
        Self {
            handle,
            session,
            store,
            tx_idle_timeout,
            escalation_timeout,
            offload_rearms: Arc::default(),
            offload_nudges: Arc::default(),
        }
    }

    pub fn id(&self) -> uhlc::ID {
        self.session.zid().into()
    }

    pub fn find_tx(&self, tx_id: models::TxId) -> Option<RemoteTx<'_, TxEvents>> {
        let (high, low, node) = tx_id;
        debug_assert_eq!(
            node,
            self.id().to_le_bytes(),
            "tx_id originated from a different node",
        );
        let tx_id = uuid::Uuid::from_u64_pair(high, low);

        self.store.find_remote_tx(tx_id)
    }

    pub fn remove_tx(
        &self,
        tx_id: models::TxId,
    ) -> Option<db::store::fjall::Transaction<TxEvents>> {
        let (high, low, node) = tx_id;
        debug_assert_eq!(
            node,
            self.id().to_le_bytes(),
            "tx_id originated from a different node",
        );
        let tx_id = uuid::Uuid::from_u64_pair(high, low);

        self.store.remove_remote_tx(tx_id)
    }

    pub async fn publish_events(&self, events: TxEvents, version: models::Version) {
        for (scope, table) in events {
            let ke = db_commons::topics::events::format_event(
                &scope.namespace,
                &scope.database,
                &scope.schema,
                &table,
            );

            let event = models::events::TableEvent::Inserted(version);

            let payload = match postcard::to_allocvec(&event) {
                Ok(payload) => payload,
                Err(err) => {
                    tracing::error!("unable to serialise table event: {}", err);
                    continue;
                }
            };

            if let Err(err) = self.session.put(&ke, payload).await {
                tracing::warn!("unable to publish table event [{}]: {}", ke, err);
            }
        }
    }

    fn new_remote_tx(&self, opts: TransactionOptions) -> anyhow::Result<models::TxId> {
        let me = self.id();

        let tx_id = self.store.begin_remote(&opts)?;
        tracing::trace!("[{}] Starting transaction: {}", me, tx_id);

        let (hi, lo) = tx_id.as_u64_pair();
        let tx_id = (hi, lo, me.to_le_bytes());

        Ok(tx_id)
    }

    /// Pokes the escalation re-arm of a running offloader for `scope`, if any.
    /// Arms an [`OffloadKind::Unwinding`] drain's timer; a drain whose timer is
    /// already armed ignores it.
    pub fn rearm_offload(&self, scope: &models::Scope) {
        let rearms = self.offload_rearms.lock().expect("rearm map poisoned");
        if let Some(rearm) = rearms.get(scope) {
            rearm.notify_one();
        }
    }

    /// Wakes the drain of `scope`, if any, so it announces the rows a commit
    /// just left it holding instead of waiting out its periodic tick. A burst
    /// of commits coalesces into one wake, so this cannot turn write traffic
    /// into announce traffic one-for-one.
    pub fn nudge_offload(&self, scope: &models::Scope) {
        let nudges = self.offload_nudges.lock().expect("nudge map poisoned");
        if let Some(nudge) = nudges.get(scope) {
            nudge.notify_one();
        }
    }

    /// Announces and serves `scope` on its replica channel so the nodes that
    /// replicate it pull the data off this one. Local data is kept; the
    /// offloader retires once a full replica announces it holds everything we
    /// do, or once this node starts replicating the scope itself. If nothing
    /// covers it within the escalation timeout (armed per `kind`), it promotes
    /// itself into a custody-backed replica rather than serve the data out
    /// forever — the safety valve that gives data a durable home.
    ///
    /// A retiring [`OffloadKind::Unwinding`] drain deletes this node's custody
    /// row: the coverage confirmation is what ends the custody it recorded.
    pub fn start_offload(&self, scope: models::Scope, kind: OffloadKind) {
        let subject = models::Subject::Scope(scope.clone());
        let transport =
            replication::ZenohTransport::new(&self.session, &subject, &self.store, "offload");

        let replica_client = transport.client.clone();

        let Some(repl) = self.store.offload(transport, scope.clone()) else {
            // Already offloading, or replication covers the scope.
            return;
        };

        // Inbound: serve changeset requests and direct pulls, watch announces
        // for coverage.
        self.handle.spawn({
            let handle = self.handle.clone();
            let repl = repl.clone();
            let context = self.clone();
            let subject = subject.clone();

            async move {
                // Held until the drain stops, then dropped to undeclare it.
                let _sync_queryable = context.declare_sync(&subject, repl.clone()).await;

                let _sub = replica_client
                    .subscribe({
                        let repl = repl.clone();
                        let handle = handle;

                        move |sender, msg| {
                            let kind = msg.name();
                            metrics::record_msg_recv(kind);

                            // Taken before the spawn, so the gap to the task
                            // actually running is measured rather than assumed.
                            let queued_at = std::time::Instant::now();
                            let fut = repl.clone().handle_message(sender, msg);

                            handle.spawn(async move {
                                let queued = queued_at.elapsed();
                                let started = std::time::Instant::now();
                                fut.await;
                                metrics::record_handled(kind, queued, started.elapsed());
                            });
                        }
                    })
                    .await;

                repl.stopped().await;
            }
        });

        let rearm = Arc::new(Notify::new());
        self.offload_rearms
            .lock()
            .expect("rearm map poisoned")
            .insert(scope.clone(), rearm.clone());

        let nudge = Arc::new(Notify::new());
        self.offload_nudges
            .lock()
            .expect("nudge map poisoned")
            .insert(scope.clone(), nudge.clone());

        self.handle
            .spawn(drive_offload(self.clone(), repl, scope, kind, rearm, nudge));
    }

    /// Declares the direct catch-up queryable for `subject`: answers pull
    /// pages and coverage checks against this holder.
    async fn declare_sync(
        &self,
        subject: &models::Subject,
        repl: db::replication::Replicator<replication::ZenohTransport, TxEvents>,
    ) -> zenoh::query::Queryable<()> {
        use db_commons::models::replication::sync;

        let me = self.session.zid();
        let (namespace, database, schema) = subject.as_keyexprs();
        let ke = db_commons::topics::replica_sync::format(me, namespace, database, schema);

        let handle = self.handle.clone();
        let store = self.store.clone();
        self.session
            .declare_queryable(ke)
            .callback(move |query| {
                let repl = repl.clone();
                let store = store.clone();
                handle.spawn(async move {
                    let Some(payload) = query.payload() else {
                        return;
                    };
                    let req = match postcard::from_bytes::<sync::Request>(&payload.to_bytes()) {
                        Ok(req) => req,
                        Err(err) => {
                            tracing::warn!("undecodable sync request: {}", err);
                            return;
                        }
                    };

                    let pull_namespace = match &req {
                        sync::Request::Pull(req) => Some(req.scope.namespace.clone()),
                        sync::Request::Verify(_) => None,
                    };

                    // Store scans, off the async workers: a busy sync
                    // queryable must not starve the locate/tx queryables
                    // sharing the executor.
                    let served = tokio::task::spawn_blocking(move || match req {
                        sync::Request::Pull(req) => repl
                            .serve_pull(&req, db::replication::SYNC_PAGE_BYTES)
                            .map(sync::Response::Pull),
                        sync::Request::Verify(req) => repl.verify_coverage(&req).map(|covered| {
                            sync::Response::Verify(sync::VerifyResponse { covered })
                        }),
                    })
                    .await;

                    let resp = match served {
                        Ok(resp) => resp,
                        Err(err) => {
                            tracing::warn!("sync serve task failed: {}", err);
                            return;
                        }
                    };

                    match resp {
                        Ok(resp) => {
                            if let (Some(namespace), sync::Response::Pull(page)) =
                                (&pull_namespace, &resp)
                            {
                                metrics::record_pull_served(
                                    namespace,
                                    store.now(),
                                    page.chunks.iter().map(|chunk| chunk.id.1),
                                    page.next.is_some(),
                                );
                            }

                            let bytes = postcard::to_allocvec(&resp)
                                .expect("unable to serialise sync response");
                            if let Err(err) = query.reply(query.key_expr().clone(), bytes).await {
                                tracing::warn!("unable to reply to a sync request: {}", err);
                            }
                        }
                        Err(err) => tracing::warn!("unable to serve a sync request: {}", err),
                    }
                });
            })
            .await
            .expect("unable to declare sync queryable")
    }

    /// Declares the locate queryable answering "who holds this scope?" for
    /// `locate_ke`, replying with this node's `state`.
    async fn declare_locate(
        &self,
        locate_ke: String,
        state: models::locate::HolderState,
    ) -> zenoh::query::Queryable<()> {
        let store = self.store.clone();
        let handle = self.handle.clone();
        let node_id: models::NodeId = self.id().to_le_bytes();

        self.session
            .declare_queryable(locate_ke)
            .callback(move |query| {
                let store = store.clone();
                handle.spawn(async move { handle_locate(&store, node_id, state, query).await });
            })
            .await
            .expect("unable to declare locate queryable")
    }

    /// Makes this node a provisional replica for `scope`: records its own
    /// custody row — lineage that keeps the promotion forever collapsible —
    /// and starts replicating immediately.
    ///
    /// Used when a routed transaction lands here as a fallback because no
    /// replica could be located — the write must leave a replica behind, or
    /// the scope's data would stay stranded on whatever node it hit. The
    /// configured replication sets are never touched: intent is human-only,
    /// and the custody watcher collapses this custody once a configured
    /// replica (or a rendezvous-favoured fellow provisional) covers the scope.
    pub async fn promote(&self, scope: &models::Scope) -> anyhow::Result<()> {
        use cell_protocol::replication::{CUSTODY_TABLE, CustodyRow, replication_scope};

        let row = CustodyRow::new(scope.clone(), self.id().to_le_bytes());
        let key = row.key();

        let config = replication_scope();
        let config_key =
            db::domain::Key::new_scope(&config.namespace, &config.database, &config.schema);

        let mut tx = self.store.begin_local(&TransactionOptions::write())?;
        let table = config_key.table(CUSTODY_TABLE);

        if tx.tb_get(table, key.as_bytes())?.is_some() {
            // Re-promotion of a lineage this node already recorded.
            tx.rollback();
        } else {
            let value = postcard::to_allocvec(&row)?;
            tx.tb_insert(table, key.as_bytes(), &value)?;

            let version = tx.timestamp().get_time().as_u64();
            tx.commit()?;

            // The same event a remote commit to the table would raise, so
            // custody watchers re-read the table promptly.
            let events = HashSet::from([(config, String::from(CUSTODY_TABLE))]);
            self.publish_events(events, version).await;
        }

        self.start_replication(models::Subject::Scope(scope.clone()))
            .await;

        Ok(())
    }

    /// Deletes this node's custody row for `scope`, ending the recorded
    /// lineage: the custody either retired (a verified holder covers it) or
    /// converted (a human pinned this node, so configuration carries it now).
    pub async fn delete_own_custody_row(&self, scope: &models::Scope) -> anyhow::Result<()> {
        use cell_protocol::replication::{CUSTODY_TABLE, CustodyRow, replication_scope};

        let key = CustodyRow::new(scope.clone(), self.id().to_le_bytes()).key();

        let config = replication_scope();
        let config_key =
            db::domain::Key::new_scope(&config.namespace, &config.database, &config.schema);

        let mut tx = self.store.begin_local(&TransactionOptions::write())?;
        let table = config_key.table(CUSTODY_TABLE);

        if tx.tb_get(table, key.as_bytes())?.is_none() {
            tx.rollback();
            return Ok(());
        }

        tx.tb_delete(table, key.as_bytes())?;

        let version = tx.timestamp().get_time().as_u64();
        tx.commit()?;

        let events = HashSet::from([(config, String::from(CUSTODY_TABLE))]);
        self.publish_events(events, version).await;

        Ok(())
    }

    pub async fn start_replication(&self, subject: models::Subject) {
        let me = self.session.zid();
        let transport =
            replication::ZenohTransport::new(&self.session, &subject, &self.store, "replica");

        let replica_client = transport.client.clone();

        // Built before `subject` is consumed by `replicate`.
        let (namespace, database, schema) = subject.as_keyexprs();
        let locate_ke = db_commons::topics::replica_query::format(namespace, database, schema);
        let repl_subject = subject.clone();

        let Some(repl) = self.store.replicate(transport, subject) else {
            // No changes needed.
            return;
        };

        // A client-facing queryable answering "who holds this scope at >= V?".
        // Declared per replicated subject so zenoh routes locate queries only to
        // nodes replicating a covering subject. Declared eagerly (before we
        // return) so a caller that has just requested replication can rely on it.
        let queryable = self
            .declare_locate(locate_ke, models::locate::HolderState::Replica)
            .await;

        // Answers direct pulls and the coverage checks draining offloaders
        // retire on; held alongside the locate queryable.
        let sync_queryable = self.declare_sync(&repl_subject, repl.clone()).await;

        // @TODO (peeriot/swarm#788) jezza - 01 Apr 2026: I'm basically recreating a task manager here.
        //  I'd love to have a general task manager, and be able to tie it to something.
        //  Maybe we could have a future be passed into the replicate function, and tie that with replicator.

        // This handles outgoing messages, and will shutdown when the replicator is shutting down.
        self.handle.spawn({
            let handle = self.handle.clone();
            let repl = repl.clone();

            async move {
                // Held until the replicator stops, then dropped to undeclare them.
                let _queryable = queryable;
                let _sync_queryable = sync_queryable;
                let repl = repl;
                let handle = handle;

                let _sub = replica_client
                    .subscribe({
                        let repl = repl.clone();
                        let handle = handle;

                        move |sender, msg| {
                            let kind = msg.name();
                            metrics::record_msg_recv(kind);

                            // Taken before the spawn, so the gap to the task
                            // actually running is measured rather than assumed.
                            let queued_at = std::time::Instant::now();
                            let fut = repl.clone().handle_message(sender, msg);

                            handle.spawn(async move {
                                let queued = queued_at.elapsed();
                                let started = std::time::Instant::now();
                                fut.await;
                                metrics::record_handled(kind, queued, started.elapsed());
                            });
                        }
                    })
                    .await;

                repl.stopped().await;
            }
        });

        // This "forces" the replicator to announce itself periodically.
        // Like the publisher, this will kill itself when the replicator shutsdown.
        self.handle.spawn({
            let repl = repl.clone();

            async move {
                let interval = Duration::from_secs(2);
                let jitter_range = 100..6000u64;

                loop {
                    tracing::trace!("[{}] Sending announcement", me);

                    if let Err(err) = repl.announce().await {
                        tracing::warn!("unable to announce self to network: {}", err);
                    }

                    // This is good enough for now, as I'd imagine we'd be expanding on this as we develop more features.
                    // ie, Add a way for the message handler to inform the announce timer to extend it's sleep time.
                    // if we haven't received a new message on the replication channel for some time, chances are the node/scope is inactive,
                    // and we can drastically reduce the amount announces we send out.

                    let ms = rand::random_range(jitter_range.clone());
                    let sleep_time = interval + Duration::from_millis(ms);

                    tracing::trace!("[{}] sleeping for {}s", me, sleep_time.as_secs_f32());

                    tokio::select! {
                        () = tokio::time::sleep(sleep_time) => (),
                        () = repl.stopped() => break,
                    }
                }
            }
        });
    }
}

/// How often to run a direct, page-limited coverage check against a live
/// replica — cheap, but not free, so polled sparsely.
///
/// Wall-clock rather than counted in loop iterations: the loop now also wakes
/// whenever a commit strands rows, and how often coverage is checked should not
/// depend on how much traffic a scope is taking. Matches the cadence the old
/// count gave at the base interval (8 ticks x 2s).
const VERIFY_EVERY: Duration = Duration::from_secs(16);

/// A commit that strands rows wakes the drain instead of leaving them for the
/// periodic tick — but a burst of commits must not become a burst of announces,
/// which is how the mesh has been wedged before. `Notify` coalesces everything
/// arriving while the drain is busy into a single wake, and this window bounds
/// what is left: at most one extra announce per window per scope.
const NUDGE_DEBOUNCE: Duration = Duration::from_millis(50);

/// The drain loop of one offloader: announces, watches for coverage and the
/// escalation deadline, and — when a demoted provisional retires on confirmed
/// coverage — deletes the custody row its promotion recorded.
async fn drive_offload(
    context: StoreContext,
    repl: db::replication::Replicator<replication::ZenohTransport, TxEvents>,
    scope: models::Scope,
    kind: OffloadKind,
    rearm: Arc<Notify>,
    nudge: Arc<Notify>,
) {
    let me = context.session.zid();

    // Findable unless hidden, so a scope no other replica holds can still be
    // located; held for the offloader's lifetime.
    let _queryable = if matches!(kind, OffloadKind::Hidden) {
        None
    } else {
        let locate_ke = db_commons::topics::replica_query::format(
            &scope.namespace,
            &scope.database,
            &scope.schema,
        );
        Some(
            context
                .declare_locate(locate_ke, models::locate::HolderState::Draining)
                .await,
        )
    };

    // A fresh drain's peer view is empty until a replica's next periodic
    // announce, which can be half a minute out. Solicit full announces now so
    // a fallback-minted sink learns of a live replica within a round trip —
    // from then on the quiesce check refuses routed writes instead of
    // absorbing strays blind.
    repl.solicit(&scope).await;

    let interval = Duration::from_secs(2);
    let jitter_range = 100..2000u64;
    let mut escalate_at = match kind {
        OffloadKind::Hidden | OffloadKind::Sink => {
            Some(tokio::time::Instant::now() + context.escalation_timeout)
        }
        // Unarmed: re-promotion would ping-pong with the target unless the
        // target is actually gone, which only a re-arm poke evidences.
        OffloadKind::Unwinding { .. } => None,
    };

    // Coverage-confirmed retirement is the only exit that ends a recorded
    // custody; the others leave this node a replica.
    let mut retired = false;

    // Exactly what a coverage confirmation was about, snapshotted before the
    // ask. Everything else this node holds for the scope by the time the drain
    // exits — rows a commit landed while the peer was answering, or in the
    // window after confirm_shutdown — is unattested and stays put.
    let mut covered = Vec::new();

    let mut verify_at = tokio::time::Instant::now() + VERIFY_EVERY;

    loop {
        // A replicator for the scope announces and serves strictly more than
        // this offloader; stand down.
        if context.store.is_replicating(&scope) {
            repl.confirm_shutdown();
            break;
        }

        tracing::trace!("[{}] announcing offload of {}", me, scope);

        if let Err(err) = repl.announce().await {
            tracing::warn!("unable to announce offload: {}", err);
        }

        if tokio::time::Instant::now() >= verify_at {
            verify_at = tokio::time::Instant::now() + VERIFY_EVERY;

            // Taken before the ask, so the answer can only be about more than
            // this and never less — a row that lands while the peer is
            // answering is not something it just vouched for.
            let held = held_sync_points(&context, &scope).await;

            if verify_covered(&context, &repl, &scope).await {
                // A verified retirement, same as the announce-based path: the
                // custody row this drain recorded is over. confirm_shutdown
                // drives the loop's exit through the stopped() arm below.
                retired = true;
                covered = held;
                repl.confirm_shutdown();
            }
        }

        let ms = rand::random_range(jitter_range.clone());

        tokio::select! {
            () = tokio::time::sleep(interval + Duration::from_millis(ms)) => (),
            () = repl.stopped() => {
                retired = true;
                break;
            }
            // Uncovered for too long: no replica is coming for this data, so
            // become its durable home.
            () = escalation(escalate_at) => {
                tracing::info!(
                    "[{}] escalating uncovered offload of {} to a replica",
                    me,
                    scope,
                );
                match context.promote(&scope).await {
                    // Now the scope's durable home: retire the offloader so it
                    // stops being reported as offloading, which would
                    // otherwise pin the scope out of GC forever.
                    Ok(()) => repl.confirm_shutdown(),
                    Err(err) => tracing::warn!(
                        "unable to escalate offload of {}: {}",
                        scope,
                        err,
                    ),
                }
                break;
            }
            // Rows landed for this scope: announce now rather than leaving
            // them to sit out the rest of the interval. This is the whole
            // latency path for a transaction that writes a scope its own node
            // does not hold — measured at ~4s per hop on the rack, which is one
            // tick of this loop, not any transfer cost.
            () = nudge.notified() => {
                tokio::time::sleep(NUDGE_DEBOUNCE).await;
            }
            () = rearm.notified() => {
                if escalate_at.is_none() {
                    tracing::info!("[{}] re-arming escalation for the drain of {}", me, scope);
                    escalate_at = Some(tokio::time::Instant::now() + context.escalation_timeout);
                }
            }
        }
    }

    {
        // Only this drain's own entry: a successor for the same scope may
        // already have registered its own signal.
        let mut rearms = context.offload_rearms.lock().expect("rearm map poisoned");
        if rearms.get(&scope).is_some_and(|n| Arc::ptr_eq(n, &rearm)) {
            rearms.remove(&scope);
        }

        let mut nudges = context.offload_nudges.lock().expect("nudge map poisoned");
        if nudges.get(&scope).is_some_and(|n| Arc::ptr_eq(n, &nudge)) {
            nudges.remove(&scope);
        }
    }

    // A verified holder covers everything this drain held; its custody — if
    // this was a demoted provisional — is over.
    if retired
        && matches!(kind, OffloadKind::Unwinding { .. })
        && let Err(err) = context.delete_own_custody_row(&scope).await
    {
        tracing::warn!("unable to delete the custody row of {}: {}", scope, err);
    }

    // Offloading a scope means ceasing to hold it. Only `retired` earns this:
    // it is the one exit where a holder was *verified* to cover everything
    // here, and the others (escalation, standing down to a real replicator)
    // deliberately leave this node holding the scope. Kept out of the loop so
    // it runs after the replicator has confirmed shutdown and can no longer
    // serve a pull from what we are about to drop.
    //
    // `covered` is empty unless a verification actually happened, so a
    // `stopped()` that came from somewhere else releases nothing.
    if retired {
        release_offloaded(&context, &scope, &covered, me).await;
    }
}

/// The sync points this node holds for `scope`, or an empty snapshot if the
/// scan fails — releasing nothing is always the safe answer.
async fn held_sync_points(
    context: &StoreContext,
    scope: &models::Scope,
) -> Vec<models::SyncPointId> {
    let store = context.store.clone();
    let scanned = tokio::task::spawn_blocking({
        let scope = scope.clone();
        move || store.held_sync_points(&scope)
    })
    .await;

    match scanned {
        Ok(Ok(points)) => points,
        Ok(Err(err)) => {
            tracing::error!("unable to snapshot the sync points of {scope}: {err}");
            Vec::new()
        }
        Err(err) => {
            tracing::error!("sync point scan for {scope} failed: {err}");
            Vec::new()
        }
    }
}

/// Drops what a verified holder was confirmed to cover — see
/// [`Store::release_scope`](db::store::fjall::Store::release_scope) for why
/// keeping it is what made every command arrive more than once, and why only
/// the confirmed `points` go rather than everything here now.
///
/// Only ever called on the coverage-verified exit.
async fn release_offloaded(
    context: &StoreContext,
    scope: &models::Scope,
    points: &[models::SyncPointId],
    me: zenoh::config::ZenohId,
) {
    let store = context.store.clone();
    let released = tokio::task::spawn_blocking({
        let scope = scope.clone();
        let points = points.to_vec();
        move || store.release_scope(&scope, &points)
    })
    .await;

    match released {
        Ok(Ok(0)) => (),
        Ok(Ok(count)) => {
            tracing::info!("[{me}] released {count} offloaded sync point(s) of {scope}");
        }
        Ok(Err(err)) => tracing::error!("unable to release the offloaded data of {scope}: {err}"),
        Err(err) => tracing::error!("release task for {scope} failed: {err}"),
    }
}

/// Whether any live replica in the peer view confirms full coverage of `scope`.
/// A true result means this drain's data is safely held elsewhere and the
/// offload can retire.
async fn verify_covered(
    context: &StoreContext,
    repl: &db::replication::Replicator<replication::ZenohTransport, TxEvents>,
    scope: &models::Scope,
) -> bool {
    let me = context.session.zid();
    let replicas = context
        .store
        .peer_view(scope, tokio::time::Instant::now().into_std())
        .into_iter()
        .filter(|peer| matches!(peer.state, models::locate::HolderState::Replica));

    for peer in replicas {
        let Ok(id) = uhlc::ID::try_from(peer.id) else {
            continue;
        };

        if matches!(repl.confirm_covered_by(id, scope).await, Some(true)) {
            tracing::info!(
                "[{}] {} verified as fully held by [{}]; offload complete",
                me,
                scope,
                id,
            );
            return true;
        }
    }
    false
}

/// Resolves when the escalation deadline passes; never, when it is unarmed.
async fn escalation(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Answers a locate query: reply with our node id and head version iff we have
/// applied the commit at the requested `min_version` (not merely a later one —
/// during catch-up the head can outrun a still-missing earlier version). A node
/// that does not qualify stays silent, so the client only ever sees eligible
/// holders. The reported head is still the max present version, for ranking.
///
/// A replica holding no data yet answers unversioned queries at head 0: it is
/// still the right routing target for a scope's first write.
async fn handle_locate(
    store: &db::store::fjall::Store<TxEvents>,
    node_id: models::NodeId,
    state: models::locate::HolderState,
    query: zenoh::query::Query,
) {
    let (namespace, database, schema) =
        match db_commons::topics::replica_query::parse_scope(query.key_expr().as_str()) {
            Ok(parts) => parts,
            Err(err) => {
                tracing::warn!(
                    "dropping malformed locate keyexpr [{}]: {}",
                    query.key_expr(),
                    err
                );
                return;
            }
        };
    let scope = models::Scope::new(namespace, database, schema);

    let Some(req) = db_commons::query::parse_query::<models::locate::Request>(&query) else {
        // parse_query logs the failure.
        return;
    };

    // Store reads off the async workers, like `declare_sync`'s scans: the
    // data-plane handlers run blocking store work on the same runtime, and a
    // locate task stuck behind them in the run queue looks, to the whole
    // mesh, like "no holder" (run 33257380391: ~8,000 empty locate rounds in
    // one 25 s pass while every holder was alive and static). Replies need no
    // explicit QoS here — they inherit the query's priority.
    let resolved = tokio::task::spawn_blocking({
        let store = store.clone();
        let scope = scope.clone();

        move || {
            // Qualify on exact presence of the requested version, not
            // `head >= min`: during catch-up the head (max present version)
            // can outrun a lower version that is still missing, so a `>=`
            // check would wrongly claim it.
            if let Some(min) = req.min_version {
                match store.scope_has_version(&scope, min) {
                    Ok(true) => {}
                    // Behind, or holding a gap at the requested version; stay silent.
                    Ok(false) => return Ok(None),
                    Err(err) => return Err(err),
                }
            }

            let head = match store.scope_head_version(&scope) {
                Ok(Some(head)) => head,
                // Empty, but replicating (or this queryable wouldn't exist). Only an
                // unversioned query reaches here; a versioned one qualified above.
                Ok(None) => 0,
                Err(err) => return Err(err),
            };

            Ok(Some((
                head,
                store.peer_view(&scope, std::time::Instant::now()),
            )))
        }
    })
    .await;

    let (head, peers) = match resolved {
        Ok(Ok(Some(resolved))) => resolved,
        Ok(Ok(None)) => return,
        Ok(Err(err)) => {
            tracing::error!("unable to resolve scope state for locate: {}", err);
            return;
        }
        Err(err) => {
            tracing::error!("locate store-read task failed: {}", err);
            return;
        }
    };

    let response = models::locate::Response {
        id: node_id,
        head,
        peers,
        state,
    };
    let payload = match postcard::to_allocvec(&response) {
        Ok(payload) => payload,
        Err(err) => {
            tracing::error!("unable to serialise locate response: {}", err);
            return;
        }
    };

    if let Err(err) = query.reply(query.key_expr(), payload).await {
        tracing::warn!("unable to reply to locate query: {}", err);
    }
}
