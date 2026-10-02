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
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

mod pacer;
use pacer::PullPacer;

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

    /// Publishes an announce; `reason` is why it went out, for metrics.
    fn publish_announce(
        &self,
        reason: AnnounceReason,
        announce: Announce,
    ) -> impl Future<Output = ()> + Send {
        let _ = reason;
        self.publish(ReplicaMessage::Announce(announce))
    }

    /// Publishes a probe; `reason` is why it went out, for metrics.
    fn publish_probe(&self, reason: ProbeReason, probe: Probe) -> impl Future<Output = ()> + Send {
        let _ = reason;
        self.publish(ReplicaMessage::Probe(probe))
    }
}

/// Why a probe went out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeReason {
    /// Starting up: whoever holds the scope, speak now.
    Solicit,
    /// A peer's announce diverged from ours in a way only a full announce
    /// resolves.
    Repair,
    /// A gossip-only offloader asking for the full announces that retire it.
    Gossip,
}

impl ProbeReason {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Solicit => "solicit",
            Self::Repair => "repair",
            Self::Gossip => "gossip",
        }
    }
}

/// Why an announce went out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnounceReason {
    /// The announce loop's periodic tick.
    Tick,
    /// A commit woke the announce loop.
    Commit,
    /// A peer's probe asked for it.
    Probe,
    /// A pull from a drain finished.
    Pull,
    /// The announce loop woke for anything else.
    Wake,
}

impl AnnounceReason {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Tick => "tick",
            Self::Commit => "commit",
            Self::Probe => "probe",
            Self::Pull => "pull",
            Self::Wake => "wake",
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

/// The heads this node holds for a subject, as of one scan.
struct Frontiers {
    /// When the scan finished; a request made before this reuses it.
    scanned_at: Instant,
    /// The scanning transaction's timestamp — "now" for the announce lag cut.
    now: uhlc::Timestamp,
    scopes: VecMap<api::Scope, domain::ScopeFrontier>,
}

/// The latest frontier scan of one subject, shared across a replicator's
/// clones so that at most one scan is in flight for it at a time.
#[derive(Default)]
struct FrontierCache {
    latest: tokio::sync::Mutex<Option<Arc<Frontiers>>>,
    scans: AtomicU64,
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
    frontier: Arc<FrontierCache>,
    /// What an offloader's announce-based retirement was confirmed to cover:
    /// the frontier a full replica's announce vouched for, version by version.
    covered: Arc<std::sync::Mutex<Vec<models::SyncPointId>>>,
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
            frontier: self.frontier.clone(),
            covered: self.covered.clone(),
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
            frontier: Default::default(),
            covered: Default::default(),
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

    /// The sync points an announce-based retirement confirmed a full replica
    /// holds, taken so a retired drain can release exactly those. Empty unless
    /// this offloader retired that way.
    pub fn take_confirmed_coverage(&self) -> Vec<models::SyncPointId> {
        std::mem::take(&mut *self.covered.lock().expect("coverage lock poisoned"))
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
        self.announce_for(AnnounceReason::Tick).await
    }

    /// [`Self::announce`], saying why.
    pub async fn announce_for(&self, reason: AnnounceReason) -> anyhow::Result<()> {
        match self.mode {
            ReplicaMode::Full => self.send_announce(&[], true, reason).await.map(drop),
            // With a sync-capable transport, replicas pull a drain's holdings
            // directly and coverage is verified point-to-point, so a floored
            // heartbeat is all the mesh needs — re-broadcasting a frozen
            // frontier in full every round is pure waste.
            ReplicaMode::Offload if self.transport.can_sync() => {
                self.send_announce(&[], true, reason).await.map(drop)
            }
            // Gossip-only: an offloader holds a stray subset, so a baseline
            // fingerprint would never match a replica's; it announces
            // explicitly, and the probe solicits the full announces its
            // announce-based coverage check needs.
            ReplicaMode::Offload => {
                let scopes = self.send_announce(&[], false, reason).await?;
                if !scopes.is_empty() {
                    self.transport
                        .publish_probe(ProbeReason::Gossip, Probe { filter: scopes })
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
                ReplicaMode::Offload => self.handle_coverage(sender, announce).await,
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
        self.send_announce(&probe.filter, false, AnnounceReason::Probe)
            .await
            .map(drop)
    }

    /// The heads this node holds for its subject.
    ///
    /// The scan walks every sync point of the subject, so it runs off the
    /// async workers, one at a time per replicator; every request that arrives
    /// while a scan runs shares its result. A burst of peer announces thereby
    /// costs one scan rather than one each, and a result is at most one scan
    /// duration stale — the window a concurrent commit already has against an
    /// in-flight scan, and one a peer's next announce closes.
    async fn frontiers(&self) -> anyhow::Result<Arc<Frontiers>> {
        let requested = Instant::now();
        let mut latest = self.frontier.latest.lock().await;
        if let Some(cached) = latest.as_ref().filter(|c| c.scanned_at >= requested) {
            return Ok(cached.clone());
        }

        let store = self.store.clone();
        let subject = self.subject.clone();
        let (now, scopes) = tokio::task::spawn_blocking(move || {
            let tx = store
                .begin_local(&TransactionOptions::read())
                .context("unable to start transaction")?;

            let (lower, upper) = domain::SyncPoint::range_from_subject(&subject)?;

            let mut scopes = VecMap::<api::Scope, domain::ScopeFrontier>::new();
            tx.collect_latest_heads(lower, upper, |scope, id, _| {
                let (epoch, v, node_id) = id;

                let duplicate_entry = scopes
                    .entry(scope)
                    .or_default()
                    .insert(v, (epoch, node_id))
                    .is_some();

                if duplicate_entry {
                    anyhow::bail!("collect_latest_heads emitted duplicate ts for one scope");
                }

                Ok(())
            })?;

            anyhow::Ok((tx.timestamp(), scopes))
        })
        .await
        .context("frontier scan aborted")??;

        self.frontier.scans.fetch_add(1, Ordering::Relaxed);
        let scanned = Arc::new(Frontiers {
            scanned_at: Instant::now(),
            now,
            scopes,
        });
        *latest = Some(scanned.clone());

        Ok(scanned)
    }

    /// How many frontier scans this replicator has run; requests served from a
    /// scan already in flight don't count.
    pub fn frontier_scans(&self) -> u64 {
        self.frontier.scans.load(Ordering::Relaxed)
    }

    async fn send_announce(
        &self,
        filter: &[api::Scope],
        floored: bool,
        reason: AnnounceReason,
    ) -> anyhow::Result<Vec<api::Scope>> {
        let me = self.store.node_id();

        let frontiers = self.frontiers().await?;
        let announced: Vec<_> = frontiers
            .scopes
            .iter()
            .filter(|(scope, _)| filter.is_empty() || filter.contains(scope))
            .collect();

        tracing::trace!("[{}] announce has {} scope(s)", me, announced.len());

        let lag_cut = floored.then(|| {
            let now = frontiers.now.get_time().0;
            now.saturating_sub(uhlc::NTP64::from(self.lag).0)
        });

        let mut known = VecMap::<api::Scope, ScopeAnnounce>::new();
        let mut scopes = Vec::with_capacity(announced.len());
        for (scope, frontier) in announced {
            scopes.push(scope.clone());

            // The baseline is capped at the newest held head, so it never
            // vouches for versions this node has not actually seen.
            let cut = lag_cut.and_then(|cut| {
                let newest = frontier.keys().next_back().copied()?;
                Some(newest.min(cut))
            });

            let sa = match cut {
                None => ScopeAnnounce::full(frontier.clone()),
                Some(cut) => {
                    let mut sa = ScopeAnnounce {
                        baseline: Some(cut),
                        ..Default::default()
                    };
                    for (&ts, &(epoch, node_id)) in frontier {
                        if sa.elides(ts, epoch) {
                            sa.fingerprint ^= head_fingerprint(ts, epoch, &node_id);
                        } else {
                            sa.heads.insert(ts, (epoch, node_id));
                        }
                    }
                    sa
                }
            };

            known.insert(scope.clone(), sa);
        }

        self.transport
            .publish_announce(
                reason,
                Announce {
                    known,
                    full_replica: matches!(self.mode, ReplicaMode::Full),
                },
            )
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
        self.probe_scope(scope, ProbeReason::Solicit).await;
    }

    /// Probes `scope` for a full announce, unless one was requested recently.
    async fn probe_scope(&self, scope: &api::Scope, reason: ProbeReason) {
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
                .publish_probe(
                    reason,
                    Probe {
                        filter: vec![scope.clone()],
                    },
                )
                .await;
        }
    }

    async fn handle_announce(&self, sender: uhlc::ID, announce: Announce) -> anyhow::Result<()> {
        let me = self.store.node_id();
        let peer = sender.to_le_bytes();

        let frontiers = self.frontiers().await?;
        let our_frontier = &frontiers.scopes;

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
                        self.probe_scope(scope, ProbeReason::Repair).await;
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
    async fn handle_coverage(&self, sender: uhlc::ID, announce: Announce) -> anyhow::Result<()> {
        // The frontier checked is exactly what the announce vouched for, so
        // it is also exactly what a retirement on it may release.
        let covered = if announce.full_replica {
            let frontiers = self.frontiers().await?;
            let all_held = frontiers.scopes.iter().all(|(scope, frontier)| {
                frontier.iter().all(|(ts, &(epoch, _))| {
                    announce
                        .known
                        .get(scope)
                        .and_then(|sa| sa.heads.get(ts))
                        .is_some_and(|&(their_epoch, _)| their_epoch >= epoch)
                })
            });
            all_held.then(|| {
                frontiers
                    .scopes
                    .iter()
                    .flat_map(|(_, frontier)| frontier.iter())
                    .map(|(&ts, &(epoch, node))| (epoch, ts, node))
                    .collect::<Vec<_>>()
            })
        } else {
            None
        };

        self.store.record_peer_frontier(
            sender.to_le_bytes(),
            announce.known,
            announce.full_replica,
        );

        if let Some(points) = covered {
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
            // Recorded before the shutdown it justifies, so the drain sees it
            // as soon as it wakes.
            *self.covered.lock().expect("coverage lock poisoned") = points;
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
        // The requester paces this transfer; see [`PullPacer`].
        let mut pacer = PullPacer::new();
        // The oldest pulled version as a floored announce would judge it:
        // elided once both its timestamp and epoch fall behind the cut.
        let mut oldest = models::Version::MAX;

        loop {
            let page_started = Instant::now();
            let Some(sync::PullResponse { chunks, next }) =
                self.transport.pull(target, req.clone()).await
            else {
                return false;
            };

            let fetched = page_started.elapsed();
            let page_chunks = chunks.len();
            let page_bytes: usize = chunks
                .iter()
                .map(|chunk| chunk_size(&chunk.entries) + CHUNK_WIRE_OVERHEAD)
                .sum();
            for &(epoch, ts, _) in chunks.iter().map(|chunk| &chunk.id) {
                oldest = oldest.min(ts.max(epoch));
            }
            if let Err(err) = self.apply_pull(scope, chunks).await {
                // Partially applied; the holder's next announce drives a retry.
                tracing::error!("unable to apply a pulled page: {}", err);
                return true;
            }
            pages += 1;

            let took = page_started.elapsed();
            let rest = pacer.rest_after(took, page_bytes);
            tracing::debug!(
                "[{}]({}) applied pull page {} ({} chunk(s), {} bytes) from [{}]: fetched in {:?}, applied in {:?}, resting {:?}",
                me,
                scope,
                pages,
                page_chunks,
                page_bytes,
                target,
                fetched,
                took.saturating_sub(fetched),
                rest,
            );

            match next {
                Some(cursor) => {
                    req.after = Some(cursor);
                    tokio::time::sleep(rest).await;
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

        // Caught up with the drain, which retires once a full replica vouches
        // for every version it holds. A handoff — history, or more than a page
        // of it — is vouched for now. A sink's trickle of fresh rows waits for
        // our periodic announce: under load that is thousands of pulls, and
        // an announce each costs every peer a frontier scan.
        let lag_cut = self
            .store
            .now()
            .saturating_sub(uhlc::NTP64::from(self.lag).0);
        let floored = oldest > lag_cut;
        if floored && pages == 1 {
            return true;
        }
        if let Err(err) = self
            .send_announce(std::slice::from_ref(scope), floored, AnnounceReason::Pull)
            .await
        {
            tracing::warn!(
                "[{}]({}) unable to announce a finished pull: {}",
                me,
                scope,
                err
            );
        }
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

            let wrote = tx.insert_changeset(sp, sm, &entries)?;

            if tx.commit().is_ok() {
                inserted = wrote;
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
