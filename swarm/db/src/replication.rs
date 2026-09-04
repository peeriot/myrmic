use crate::domain;
use crate::domain::api;
use crate::store::TransactionOptions;
use crate::store::fjall::Store;
use anyhow::Context;
use db_commons::models;
use db_commons::models::replication::{
    Announce, ChangeSet, ChangeSetReq, Chunk, Probe, ScopeAnnounce, VecMap, head_fingerprint, sync,
};
use db_commons::models::{ReplicaMessage, Subject};
use skey::StoreKey;
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How far behind "now" the announce baseline sits. Heads younger than this
/// stay explicit, so peers that lag a few announce rounds still see them
/// (and epoch bumps) directly instead of through a probed full announce.
const ANNOUNCE_LAG: Duration = Duration::from_secs(30);

/// Minimum spacing between probes for the same scope, so a long catch-up
/// doesn't solicit a full announce on every periodic announce it receives.
const PROBE_COOLDOWN: Duration = Duration::from_secs(10);

/// Page budget for a direct catch-up pull reply — one zenoh link frame's
/// worth: a bigger reliable message occupies a slow link for whole seconds,
/// which is how zenoh's pipeline deadline gets tripped.
pub const SYNC_PAGE_BYTES: usize = 64 * 1024;

/// Wire cost a chunk carries besides its entries (id, meta, framing). Counted
/// into the pull page budget so a page of entry-less tombstone chunks still
/// pages instead of ballooning into one link-choking reply.
const CHUNK_WIRE_OVERHEAD: usize = 48;

/// Pause between pulled pages. The puller paces the transfer, so this is the
/// duty-cycle knob that keeps a deep pull from saturating the holder's link
/// or the puller's own storage.
const PULL_PAGE_PAUSE: Duration = Duration::from_millis(250);

/// Rough serialised size of a chunk's payload, for the pull page budget —
/// the entry bytes plus a small per-entry overhead.
fn chunk_size(entries: &[(models::RawKey, Option<models::Value>)]) -> usize {
    const PER_ENTRY_OVERHEAD: usize = 16;
    const PER_CHUNK_OVERHEAD: usize = 32;
    PER_CHUNK_OVERHEAD
        + entries
            .iter()
            .map(|(k, v)| k.len() + v.as_ref().map_or(0, Vec::len) + PER_ENTRY_OVERHEAD)
            .sum::<usize>()
}

pub trait ReplicaTransport: Clone + Send + Sync + 'static {
    /// Used to send outgoing messages to the other nodes.
    fn publish(&self, msg: ReplicaMessage) -> impl Future<Output = ()> + Send;

    /// Whether this transport can address a specific holder directly (pull
    /// pages, coverage checks). A gossip-only transport leaves the defaults,
    /// and callers fall back to the broadcast paths.
    fn can_sync(&self) -> bool {
        false
    }

    /// One page of a direct catch-up pull from `target`; `None` when the
    /// transport cannot query (or the query failed).
    fn pull(
        &self,
        target: uhlc::ID,
        req: sync::PullRequest,
    ) -> impl Future<Output = Option<sync::PullResponse>> + Send {
        async move {
            let _ = (target, req);
            None
        }
    }

    /// Asks `target` whether it covers a page of heads; `None` when the
    /// transport cannot query (or the query failed).
    fn verify(
        &self,
        target: uhlc::ID,
        req: sync::VerifyRequest,
    ) -> impl Future<Output = Option<bool>> + Send {
        async move {
            let _ = (target, req);
            None
        }
    }
}

#[derive(Clone)]
pub struct ReplicationHandle {
    /// Fires when the replicator is stopping.
    stopped: tokio_util::sync::CancellationToken,
}

impl ReplicationHandle {
    /// Stops the replicator. All tasks watching `stopped()` wind down; data
    /// this node still holds is the offload machinery's to drain.
    pub fn stop(&self) {
        self.stopped.cancel();
    }

    /// Returns `true` if the replicator has confirmed it is stopping.
    pub fn is_stopped(&self) -> bool {
        self.stopped.is_cancelled()
    }

    /// Waits until the replicator has confirmed it is stopping.
    pub async fn until_stopped(&self) {
        self.stopped.cancelled().await;
    }
}

/// How a replicator takes part in the protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicaMode {
    /// Pulls what it lacks and serves what it has.
    Full,
    /// Serves a scope this node holds without replicating it, so the nodes
    /// that do replicate it can pull the data off. Never pulls or applies,
    /// and retires once a full replica announces it holds everything we do.
    Offload,
}

pub struct Replicator<T: ReplicaTransport, M = ()> {
    store: Store<M>,
    stopped: tokio_util::sync::CancellationToken,
    pub(crate) transport: T,
    subject: Subject,
    mode: ReplicaMode,
    lag: Duration,
    /// Last probe per scope, shared across clones to enforce the cooldown.
    probes: Arc<dashmap::DashMap<api::Scope, Instant>>,
    /// In-flight direct pulls, keyed by (holder, scope), so overlapping
    /// announces from the same holder don't stack duplicate pulls.
    pulling: Arc<dashmap::DashMap<(models::NodeId, api::Scope), ()>>,
}

/// Holds one (holder, scope) pull slot; the slot frees on drop.
pub(crate) struct PullGuard {
    pulling: Arc<dashmap::DashMap<(models::NodeId, api::Scope), ()>>,
    key: (models::NodeId, api::Scope),
}

impl Drop for PullGuard {
    fn drop(&mut self) {
        self.pulling.remove(&self.key);
    }
}

impl<T: ReplicaTransport, M> Clone for Replicator<T, M> {
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            stopped: self.stopped.clone(),
            transport: self.transport.clone(),
            subject: self.subject.clone(),
            mode: self.mode,
            lag: self.lag,
            probes: self.probes.clone(),
            pulling: self.pulling.clone(),
        }
    }
}

impl<T: ReplicaTransport, M: Send + Sync + 'static> Replicator<T, M> {
    pub(crate) fn new(
        store: Store<M>,
        transport: T,
        subject: Subject,
        mode: ReplicaMode,
    ) -> (Self, ReplicationHandle) {
        let me = store.node_id();

        {
            let (namespace, db, schema) = subject.as_keyexprs();
            tracing::debug!("[{}] replicating {}/{}/{}", me, namespace, db, schema);
        }

        let stopped = tokio_util::sync::CancellationToken::new();

        let replicator = Self {
            store,
            stopped: stopped.clone(),
            transport,
            subject,
            mode,
            lag: ANNOUNCE_LAG,
            probes: Default::default(),
            pulling: Default::default(),
        };

        let handle = ReplicationHandle { stopped };

        (replicator, handle)
    }

    /// Confirms that this replicator is stopping. All tasks watching the
    /// handle's `stopped()` future will be notified.
    pub fn confirm_shutdown(&self) {
        self.stopped.cancel();
    }

    /// Waits until the replicator has confirmed it is stopping.
    pub async fn stopped(&self) {
        self.stopped.cancelled().await;
    }

    async fn request_changeset(
        &self,
        scope: models::Scope,
        since_ts: Option<models::Version>,
        epoch_floors: BTreeMap<models::Version, models::Epoch>,
    ) {
        self.transport
            .publish(ReplicaMessage::ChangeSetReq(ChangeSetReq {
                tx_id: None,
                scope,
                since_ts,
                epoch_floors,
            }))
            .await;
    }

    /// Shrinks the announce baseline for testing, so freshly written heads
    /// are elided instead of waiting out `ANNOUNCE_LAG`.
    pub fn set_lag(&mut self, lag: Duration) {
        self.lag = lag;
    }

    /// This tells the replica to generate an Announce message, and send it through the transport.
    pub async fn announce(&self) -> anyhow::Result<()> {
        match self.mode {
            ReplicaMode::Full => self.send_announce(&[], true).await.map(drop),
            // With a sync-capable transport, replicas pull a drain's holdings
            // directly and coverage is verified point-to-point, so a floored
            // heartbeat is all the mesh needs — re-broadcasting a frozen
            // frontier in full every round is pure waste.
            ReplicaMode::Offload if self.transport.can_sync() => {
                self.send_announce(&[], true).await.map(drop)
            }
            // Gossip-only: an offloader holds a stray subset, so a baseline
            // fingerprint would never match a replica's; it announces
            // explicitly, and the probe solicits the full announces its
            // announce-based coverage check needs.
            ReplicaMode::Offload => {
                let scopes = self.send_announce(&[], false).await?;
                if !scopes.is_empty() {
                    self.transport
                        .publish(ReplicaMessage::Probe(Probe { filter: scopes }))
                        .await;
                }
                Ok(())
            }
        }
    }

    pub async fn handle_message(self, sender: uhlc::ID, msg: ReplicaMessage) {
        let me = self.store.node_id();

        if me == sender {
            return;
        }

        tracing::debug!("[{}] Received {} from [{}]", me, msg.name(), sender);

        let result = match msg {
            ReplicaMessage::Probe(probe) => self.handle_probe(probe).await,
            ReplicaMessage::Announce(announce) => match self.mode {
                ReplicaMode::Full => self.handle_announce(sender, announce).await,
                ReplicaMode::Offload => self.handle_coverage(sender, announce),
            },
            ReplicaMessage::ChangeSetReq(req) => self.handle_cs_req(req).await,
            ReplicaMessage::ChangeSet(cs) => match self.mode {
                ReplicaMode::Full => self.handle_cs(cs).await,
                // An offloader only hands data out.
                ReplicaMode::Offload => Ok(()),
            },
        };

        if let Err(err) = result {
            tracing::error!("unable to process replica message: {}", err);
        }
    }

    /// A probe answers with a full announce: it is the repair path for peers
    /// that cannot verify a floored announce's elided prefix.
    async fn handle_probe(&self, probe: Probe) -> anyhow::Result<()> {
        self.send_announce(&probe.filter, false).await.map(drop)
    }

    async fn send_announce(
        &self,
        filter: &[api::Scope],
        floored: bool,
    ) -> anyhow::Result<Vec<api::Scope>> {
        let me = self.store.node_id();

        let tx = self
            .store
            .begin_local(&TransactionOptions::read())
            .context("unable to start transaction")?;

        let (lower, upper) = domain::SyncPoint::range_from_subject(&self.subject)?;

        let mut frontiers = VecMap::<api::Scope, domain::ScopeFrontier>::new();
        tx.collect_latest_heads(lower, upper, |scope, id, _| {
            let (epoch, v, node_id) = id;

            let duplicate_entry = frontiers
                .entry(scope)
                .or_default()
                .insert(v, (epoch, node_id))
                .is_some();

            if duplicate_entry {
                anyhow::bail!("collect_latest_heads emitted duplicate ts for one scope");
            }

            Ok(())
        })?;

        if !filter.is_empty() {
            frontiers.retain(|s, _| filter.contains(s));
        }

        tracing::trace!("[{}] announce has {} scope(s)", me, frontiers.len());

        let lag_cut = floored.then(|| {
            let now = tx.timestamp().get_time().0;
            now.saturating_sub(uhlc::NTP64::from(self.lag).0)
        });

        let mut known = VecMap::<api::Scope, ScopeAnnounce>::new();
        let mut scopes = Vec::with_capacity(frontiers.len());
        for (scope, frontier) in frontiers {
            scopes.push(scope.clone());

            // The baseline is capped at the newest held head, so it never
            // vouches for versions this node has not actually seen.
            let cut = lag_cut.and_then(|cut| {
                let newest = frontier.keys().next_back().copied()?;
                Some(newest.min(cut))
            });

            let sa = match cut {
                None => ScopeAnnounce::full(frontier),
                Some(cut) => {
                    let mut sa = ScopeAnnounce {
                        baseline: Some(cut),
                        ..Default::default()
                    };
                    for (ts, (epoch, node_id)) in frontier {
                        if sa.elides(ts, epoch) {
                            sa.fingerprint ^= head_fingerprint(ts, epoch, &node_id);
                        } else {
                            sa.heads.insert(ts, (epoch, node_id));
                        }
                    }
                    sa
                }
            };

            known.insert(scope, sa);
        }

        self.transport
            .publish(ReplicaMessage::Announce(Announce {
                known,
                full_replica: matches!(self.mode, ReplicaMode::Full),
            }))
            .await;

        Ok(scopes)
    }

    /// Solicits immediate full announces for `scope` from whoever holds it.
    ///
    /// A fresh drain's peer view starts empty and only fills on a replica's
    /// next periodic announce; soliciting brings the answer within a round
    /// trip, so a fallback-minted sink learns of a live replica (and starts
    /// refusing routed writes) after absorbing at most the write that minted
    /// it.
    pub async fn solicit(&self, scope: &api::Scope) {
        self.probe_scope(scope).await;
    }

    /// Probes `scope` for a full announce, unless one was requested recently.
    async fn probe_scope(&self, scope: &api::Scope) {
        let now = Instant::now();
        let fire = match self.probes.entry(scope.clone()) {
            dashmap::Entry::Occupied(mut entry) => {
                let due = now.duration_since(*entry.get()) >= PROBE_COOLDOWN;
                if due {
                    *entry.get_mut() = now;
                }
                due
            }
            dashmap::Entry::Vacant(entry) => {
                entry.insert(now);
                true
            }
        };

        if fire {
            self.transport
                .publish(ReplicaMessage::Probe(Probe {
                    filter: vec![scope.clone()],
                }))
                .await;
        }
    }

    async fn handle_announce(&self, sender: uhlc::ID, announce: Announce) -> anyhow::Result<()> {
        let me = self.store.node_id();
        let peer = sender.to_le_bytes();

        let tx = self
            .store
            .begin_local(&TransactionOptions::read())
            .context("unable to start transaction")?;

        let (lower, upper) = domain::SyncPoint::range_from_subject(&self.subject)?;

        let mut our_frontier = HashMap::<api::Scope, domain::ScopeFrontier>::new();
        tx.collect_latest_heads(lower, upper, |scope, id, _| {
            let (epoch, v, node_id) = id;
            our_frontier
                .entry(scope)
                .or_default()
                .insert(v, (epoch, node_id));
            Ok(())
        })?;

        let full_replica = announce.full_replica;
        let their_known = announce.known;

        for (scope, their_scope) in &their_known {
            if !self.subject.contains(scope) {
                continue;
            }

            match plan_catchup(our_frontier.get(scope), their_scope) {
                CatchupPlan::Diverged => {
                    // A non-full-replica holder (a drain) is pulled from
                    // directly: our own frontier as floors lets it serve
                    // exactly what we lack, no probe round needed. On a
                    // sync-capable transport a failed pull just waits for the
                    // drain's next announce — a gossip fallback would
                    // re-broadcast changesets mesh-wide for a transient miss.
                    if !full_replica && self.transport.can_sync() {
                        let floors = our_frontier
                            .get(scope)
                            .map(|frontier| {
                                frontier
                                    .iter()
                                    .map(|(&ts, &(epoch, _))| (ts, epoch))
                                    .collect()
                            })
                            .unwrap_or_default();
                        if !self.pull_from(sender, scope, None, floors).await {
                            tracing::debug!(
                                "[{}]({}) pull from drain [{}] failed; awaiting its next announce",
                                me,
                                scope,
                                sender,
                            );
                        }
                    } else {
                        tracing::debug!(
                            "[{}]({}) prefix diverges from peer [{}], probing for a full announce",
                            me,
                            scope,
                            sender,
                        );
                        self.probe_scope(scope).await;
                    }
                }
                CatchupPlan::Behind {
                    since_ts,
                    epoch_floors,
                } => {
                    if !full_replica && self.transport.can_sync() {
                        if !self.pull_from(sender, scope, since_ts, epoch_floors).await {
                            tracing::debug!(
                                "[{}]({}) pull from drain [{}] failed; awaiting its next announce",
                                me,
                                scope,
                                sender,
                            );
                        }
                    } else {
                        tracing::debug!(
                            "[{}]({}) behind peer [{}], requesting changeset (since_ts={:?}, floors={})",
                            me,
                            scope,
                            sender,
                            since_ts,
                            epoch_floors.len(),
                        );
                        self.request_changeset(scope.clone(), since_ts, epoch_floors)
                            .await;
                    }
                }
                CatchupPlan::CaughtUp => {
                    tracing::trace!("[{}]({}) caught up vs peer [{}]", me, scope, sender);
                }
            }
        }

        self.store
            .record_peer_frontier(peer, their_known, full_replica);

        Ok(())
    }

    /// Offload mode's announce handling: never a catch-up request. Retire once a
    /// *full replica* reports covering everything we hold for our subject, so the
    /// data verifiably lives with a node that durably retains it.
    ///
    /// A peer offloader's coverage is ignored — it only serves data out. The
    /// check is by announced frontier: a peer that has GC'd an expired version
    /// still reports its head via the retained sync-point marker, but retention
    /// is symmetric, so that version is due for deletion cluster-wide anyway.
    ///
    /// Only explicitly announced heads count: a floored announce's elided
    /// prefix cannot vouch for individual versions, so old holdings retire off
    /// the full announces our own announce's probe solicits.
    fn handle_coverage(&self, sender: uhlc::ID, announce: Announce) -> anyhow::Result<()> {
        let covered = if announce.full_replica {
            let tx = self
                .store
                .begin_local(&TransactionOptions::read())
                .context("unable to start transaction")?;

            let (lower, upper) = domain::SyncPoint::range_from_subject(&self.subject)?;

            let mut covered = true;
            tx.collect_latest_heads(lower, upper, |scope, id, _| {
                let (epoch, ts, _) = id;

                let held = announce
                    .known
                    .get(&scope)
                    .and_then(|sa| sa.heads.get(&ts))
                    .is_some_and(|&(their_epoch, _)| their_epoch >= epoch);

                covered &= held;
                Ok(())
            })?;
            covered
        } else {
            false
        };

        self.store.record_peer_frontier(
            sender.to_le_bytes(),
            announce.known,
            announce.full_replica,
        );

        if covered {
            let me = self.store.node_id();
            let (namespace, database, schema) = self.subject.as_keyexprs();
            tracing::debug!(
                "[{}] {}/{}/{} fully held by peer [{}]; offload complete",
                me,
                namespace,
                database,
                schema,
                sender,
            );
            self.confirm_shutdown();
        }

        Ok(())
    }

    /// Pulls `scope` from `target` page by page until drained, deduplicated
    /// per (target, scope). Returns `false` when the transport cannot pull —
    /// the caller falls back to broadcast gossip.
    async fn pull_from(
        &self,
        target: uhlc::ID,
        scope: &api::Scope,
        since_ts: Option<models::Version>,
        epoch_floors: BTreeMap<models::Version, models::Epoch>,
    ) -> bool {
        let me = self.store.node_id();

        let key = (target.to_le_bytes(), scope.clone());
        let _guard = {
            use dashmap::mapref::entry::Entry;
            match self.pulling.entry(key.clone()) {
                // A pull from this holder is already running; nothing to add.
                Entry::Occupied(_) => return true,
                Entry::Vacant(slot) => {
                    slot.insert(());
                    PullGuard {
                        pulling: self.pulling.clone(),
                        key,
                    }
                }
            }
        };

        let mut req = sync::PullRequest {
            scope: scope.clone(),
            after: None,
            since_ts,
            epoch_floors,
        };
        let mut pages = 0usize;

        loop {
            let Some(sync::PullResponse { chunks, next }) =
                self.transport.pull(target, req.clone()).await
            else {
                return false;
            };

            let page_chunks = chunks.len();
            if let Err(err) = self.apply_pull(scope, chunks).await {
                // Partially applied; the holder's next announce drives a retry.
                tracing::error!("unable to apply a pulled page: {}", err);
                return true;
            }
            pages += 1;
            tracing::debug!(
                "[{}]({}) applied pull page {} ({} chunk(s)) from [{}]",
                me,
                scope,
                pages,
                page_chunks,
                target,
            );

            match next {
                Some(cursor) => {
                    req.after = Some(cursor);
                    // Breathing room between pages: the requester paces this
                    // transfer, and a small node's storage must survive it.
                    tokio::time::sleep(PULL_PAGE_PAUSE).await;
                }
                None => break,
            }
        }

        tracing::debug!(
            "[{}]({}) pulled {} page(s) directly from [{}]",
            me,
            scope,
            pages,
            target,
        );
        true
    }

    /// Whether `target` covers everything this node holds for `scope`,
    /// checked page by page, newest heads first so an incomplete peer fails
    /// fast. `None` when the transport cannot ask.
    pub async fn confirm_covered_by(&self, target: uhlc::ID, scope: &api::Scope) -> Option<bool> {
        /// Heads per verify page; bounds the query payload the way the pull
        /// page budget bounds replies.
        const VERIFY_PAGE_HEADS: usize = 2048;

        // Collected off the async workers: a marker-heavy scope makes this a
        // real scan.
        let heads = {
            let store = self.store.clone();
            let scope = scope.clone();

            tokio::task::spawn_blocking(move || {
                let tx = store.begin_local(&TransactionOptions::read()).ok()?;

                let (lower, upper) = domain::Key::sync_point()
                    .namespace(&scope.namespace)
                    .database(&scope.database)
                    .schema(&scope.schema)
                    .range()
                    .ok()?;

                // Newest first: the newest head is the last thing a
                // catching-up replica acquires, so a single page usually
                // answers "not yet".
                let mut heads: Vec<(models::Version, models::Epoch)> = vec![];
                tx.collect_latest_heads(lower, upper, |_scope, id, _| {
                    let (epoch, ts, _) = id;
                    heads.push((ts, epoch));
                    Ok(())
                })
                .ok()?;
                Some(heads)
            })
            .await
            .ok()??
        };

        for page in heads.chunks(VERIFY_PAGE_HEADS) {
            let req = sync::VerifyRequest {
                scope: scope.clone(),
                heads: page.to_vec(),
            };
            if !self.transport.verify(target, req).await? {
                return Some(false);
            }
        }

        Some(true)
    }

    /// Serves one bounded page of a direct catch-up pull: this holder's sync
    /// points for `req.scope` past the cursor, minus what the requester's
    /// watermark/floors exclude, until the page holds `page_bytes` of entry
    /// data. Returns a resume cursor while more remain.
    pub fn serve_pull(
        &self,
        req: &sync::PullRequest,
        page_bytes: usize,
    ) -> anyhow::Result<sync::PullResponse> {
        if !self.subject.contains(&req.scope) {
            anyhow::bail!("scope {} is outside this holder's subject", req.scope);
        }

        let models::Scope {
            namespace,
            database,
            schema,
        } = &req.scope;
        let point = |id| {
            domain::Key::sync_point()
                .namespace(namespace)
                .database(database)
                .schema(schema)
                .with_sp_id(id)
        };

        let tx = self
            .store
            .begin_local(&TransactionOptions::read())
            .context("unable to start local transaction")?;

        let (lower, upper) = domain::Key::sync_point()
            .namespace(namespace)
            .database(database)
            .schema(schema)
            .range()
            .context("unable to construct sync point range for scope")?;
        let lower = match req.after {
            Some(id) => point(id).encode().context("unable to encode the cursor")?,
            None => lower,
        };

        let mut chunks: Vec<Chunk> = Vec::new();
        let mut bytes = 0usize;
        let mut next = None;

        tx.find_sync_points_while(lower, upper, |sp, meta| {
            let id @ (epoch, ts, _) = sp.as_id();

            // The range starts at the cursor key, which was already served.
            if req.after == Some(id) {
                return Ok(true);
            }

            let skip = match req.epoch_floors.get(&ts) {
                Some(&floor) => epoch <= floor,
                None => req.since_ts.is_some_and(|c| ts <= c),
            };
            if skip {
                return Ok(true);
            }

            if !chunks.is_empty() && bytes >= page_bytes {
                next = chunks.last().map(|c| c.id);
                return Ok(false);
            }

            let entries = match meta.marker {
                domain::SyncMarker::Deletion => vec![],
                domain::SyncMarker::Mutation => tx.changeset_for(point(id))?,
            };

            bytes = bytes
                .saturating_add(chunk_size(&entries))
                .saturating_add(CHUNK_WIRE_OVERHEAD);
            chunks.push(Chunk { id, meta, entries });
            Ok(true)
        })?;

        Ok(sync::PullResponse { chunks, next })
    }

    /// Applies one page of pulled chunks in a single transaction: a page is
    /// already size-bounded, and committing per chunk costs a journal sync per
    /// sync point — enough to bury a small node under a deep pull.
    pub async fn apply_pull(
        &self,
        scope_model: &models::Scope,
        chunks: Vec<Chunk>,
    ) -> anyhow::Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }

        let me = self.store.node_id();
        let chunks = Arc::new(chunks);

        let mut count = 0;
        loop {
            // The page's scan-and-commit runs off the async workers, so a
            // deep pull can't starve the queryables sharing the executor.
            let committed = {
                let store = self.store.clone();
                let chunks = chunks.clone();
                let scope_model = scope_model.clone();

                tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
                    let models::Scope {
                        namespace,
                        database,
                        schema,
                    } = &scope_model;
                    let scope = domain::Key::new_scope(namespace, database, schema);

                    let mut tx = store
                        .begin_local(&TransactionOptions::write())
                        .context("unable to start local transaction")?;

                    // insert_changeset skips already-present sync points, so a
                    // conflicted page retries safely.
                    for chunk in chunks.iter() {
                        let sp = domain::Key::sync_point().scope(scope).with_sp_id(chunk.id);
                        tx.insert_changeset(sp, chunk.meta, &chunk.entries)?;
                    }

                    Ok(tx.commit().is_ok())
                })
                .await
                .context("the page apply task failed")??
            };

            if committed {
                return Ok(());
            }

            count += 1;
            if count >= 10 {
                anyhow::bail!("a pulled page kept conflicting; giving up");
            }

            let jitter = rand::random_range(0..500);
            tracing::warn!(
                "[{}] pulled page for {} conflicted, retrying",
                me,
                scope_model
            );
            tokio::time::sleep(Duration::from_millis(20 + jitter)).await;
        }
    }

    /// Whether this holder covers every `(version, epoch)` in the page — each
    /// version held at the same or a newer epoch. How a draining offloader
    /// verifies a replica before retiring.
    pub fn verify_coverage(&self, req: &sync::VerifyRequest) -> anyhow::Result<bool> {
        if !self.subject.contains(&req.scope) {
            return Ok(false);
        }

        let models::Scope {
            namespace,
            database,
            schema,
        } = &req.scope;

        let tx = self
            .store
            .begin_local(&TransactionOptions::read())
            .context("unable to start local transaction")?;

        for &(ts, epoch) in &req.heads {
            let (lower, upper) = domain::Key::sync_point()
                .namespace(namespace)
                .database(database)
                .schema(schema)
                .ts(ts)
                .range()
                .context("unable to construct sync point range for version")?;

            let mut held = false;
            tx.find_sync_points_while(lower, upper, |sp, _| {
                held = sp.epoch >= epoch;
                Ok(!held)
            })?;

            if !held {
                return Ok(false);
            }
        }

        Ok(true)
    }

    async fn handle_cs_req(&self, req: ChangeSetReq) -> anyhow::Result<()> {
        let ChangeSetReq {
            tx_id,
            scope,
            since_ts,
            epoch_floors,
        } = req;

        let models::Scope {
            namespace,
            database,
            schema,
        } = &scope;

        if !self.subject.contains(&scope) {
            tracing::warn!(
                "ignoring changeset request for scope {} outside our subject",
                scope,
            );
            return Ok(());
        }

        let tx = self
            .store
            .begin_local(&TransactionOptions::read())
            .context("unable to start local transaction")?;

        let (lower, upper) = domain::Key::sync_point()
            .namespace(namespace)
            .database(database)
            .schema(schema)
            .range()
            .context("unable to construct sync point range for scope")?;

        let mut chunks = vec![];
        tx.find_sync_points(lower, upper, |sp, meta| {
            let id @ (epoch, ts, _) = sp.as_id();

            let skip = match epoch_floors.get(&ts) {
                Some(&floor) => epoch <= floor,
                None => since_ts.is_some_and(|c| ts <= c),
            };
            if skip {
                return Ok(());
            }

            let point = domain::Key::sync_point()
                .namespace(namespace)
                .database(database)
                .schema(schema)
                .with_sp_id(id);

            // A deletion marker carries no data; it tells the peer to erase
            // its own copy of the version, falling through to any older
            // version it holds.
            let entries = match meta.marker {
                domain::SyncMarker::Deletion => vec![],
                domain::SyncMarker::Mutation => tx.changeset_for(point)?,
            };

            chunks.push(Chunk { id, meta, entries });
            Ok(())
        })?;
        drop(tx);

        if chunks.is_empty() {
            return Ok(());
        }

        self.transport
            .publish(ReplicaMessage::ChangeSet(ChangeSet {
                tx_id,
                scope: scope.clone(),
                chunks,
            }))
            .await;

        Ok(())
    }

    async fn handle_cs(&self, cs: ChangeSet) -> anyhow::Result<()> {
        let ChangeSet {
            tx_id,
            scope,
            chunks,
        } = cs;

        // @TODO (peeriot/swarm#754) jezza - 01 Apr 2026: Implement this when I have a clearer idea how cross-scope transactions work.
        //  Right now, it's not needed, as the replication process will heal itself anyway.
        let _ = tx_id;

        // Apply each chunk independently: one chunk's failure must not drop the
        // rest of the batch (a batch stands in for what used to be many separate
        // messages). A dropped chunk heals on the peer's next announce.
        for chunk in chunks {
            if let Err(err) = self.apply_chunk(&scope, chunk).await {
                tracing::error!("unable to apply a changeset chunk: {}", err);
            }
        }

        Ok(())
    }

    /// Applies one chunk to the local store, retrying the commit a few times on
    /// contention before giving up on it.
    async fn apply_chunk(&self, scope_model: &models::Scope, chunk: Chunk) -> anyhow::Result<()> {
        let me = self.store.node_id();

        let Chunk {
            id: cs_sp,
            meta: sm,
            entries,
        } = chunk;

        let models::Scope {
            namespace,
            database,
            schema,
        } = scope_model;
        let scope = domain::Key::new_scope(namespace, database, schema);

        let sp = domain::Key::sync_point().scope(scope).with_sp_id(cs_sp);

        let mut count = 0;
        let mut inserted = false;
        loop {
            let tx = self.store.begin_local(&TransactionOptions::write());

            let mut tx = match tx {
                Ok(tx) => tx,
                Err(err) => {
                    tracing::error!("Failed to create transaction: {}", err);
                    break;
                }
            };

            tx.insert_changeset(sp, sm, &entries)?;

            let ok = tx.commit().is_ok();

            if ok {
                inserted = true;
                break;
            }

            count += 1;

            if count < 4 {
                tracing::error!("[{}]{} Reattempting changeset insertion", me, scope);
            } else if count < 10 {
                let jitter = rand::random_range(0..500);
                tracing::error!("[{}]{} waiting for retry", me, scope);
                tokio::time::sleep(Duration::from_millis(20 + jitter)).await;
            } else {
                tracing::error!("[{}]{} giving up", me, scope);
                break;
            }
        }

        if inserted {
            tracing::trace!("[{}]{} ingested changeset @ {:?}", me, scope, cs_sp);
        }

        Ok(())
    }
}

/// XOR-fold of the heads a floored announce at `cut` would elide.
pub(crate) fn fold_heads_at_cut(
    frontier: &domain::ScopeFrontier,
    cut: models::Version,
) -> models::replication::Fingerprint {
    frontier
        .iter()
        .filter(|&(&ts, &(epoch, _))| ts <= cut && epoch <= cut)
        .fold(0, |acc, (&ts, &(epoch, node_id))| {
            acc ^ head_fingerprint(ts, epoch, &node_id)
        })
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CatchupPlan {
    /// We hold everything the announce vouches for.
    CaughtUp,
    /// The announced prefix fold differs from ours: the sets diverge below the
    /// baseline, and only a full announce can say how.
    Diverged,
    /// The explicit heads expose gaps; pull them with the usual cursor.
    Behind {
        since_ts: Option<models::Version>,
        epoch_floors: BTreeMap<models::Version, models::Epoch>,
    },
}

pub(crate) fn plan_catchup(
    ours: Option<&domain::ScopeFrontier>,
    theirs: &ScopeAnnounce,
) -> CatchupPlan {
    if let Some(cut) = theirs.baseline {
        let fold = ours.map_or(0, |f| fold_heads_at_cut(f, cut));
        if fold != theirs.fingerprint {
            return CatchupPlan::Diverged;
        }
    }

    let mut behind = false;
    // A matching fold verifies everything below the baseline, so the cursor
    // starts there rather than at the oldest explicit head.
    let mut since_ts = theirs.baseline;
    let mut epoch_floors: BTreeMap<models::Version, models::Epoch> = BTreeMap::new();
    let mut prefix_intact = true;

    for (&ts, &(their_epoch, _their_id)) in &theirs.heads {
        let our_epoch = ours.and_then(|f| f.get(&ts)).map(|&(epoch, _)| epoch);
        let sufficient = our_epoch.is_some_and(|epoch| epoch >= their_epoch);

        if theirs.baseline.is_some_and(|b| ts <= b) {
            // An epoch spike below the cut. The cursor already claims this
            // region, so a floor is what makes the sender send it.
            if !sufficient {
                epoch_floors.insert(ts, our_epoch.unwrap_or(0));
                behind = true;
            }
            continue;
        }

        if sufficient {
            if prefix_intact {
                since_ts = Some(ts);
            } else {
                epoch_floors.insert(ts, our_epoch.expect("sufficient implies present"));
            }
        } else {
            prefix_intact = false;
            behind = true;
        }
    }

    if behind {
        CatchupPlan::Behind {
            since_ts,
            epoch_floors,
        }
    } else {
        CatchupPlan::CaughtUp
    }
}

#[cfg(test)]
mod chunk_size_tests {
    use super::chunk_size;

    #[test]
    fn chunk_size_counts_entry_bytes() {
        let empty = chunk_size(&[]);
        let one = chunk_size(&[(vec![0u8; 10], Some(vec![0u8; 20]))]);
        assert!(
            one > empty,
            "a chunk with entries costs more than an empty one"
        );
        assert!(
            one >= empty + 30,
            "the entry's key and value bytes are counted"
        );
    }
}
