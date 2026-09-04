use crate::domain::{self, Key, TransactionId};
use crate::store::{Options, TransactionOptions};

use db_commons::models;
use db_commons::models::replication::{ScopeAnnounce, SyncMarker, VecMap};

use anyhow::{Context, Result};
use skey::StoreKey;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use tx::Transaction;

mod tx;

type CMap<K, V> = Arc<dashmap::DashMap<K, V>>;

pub type RemoteTx<'a, M = ()> = dashmap::mapref::one::RefMut<'a, uuid::Uuid, Transaction<M>>;

/// How long a peer's announce stays authoritative before we treat them as
/// gone and ignore their entries when computing catch-up state.
// @TODO jezza - 29 May 2026: Ideally, this should be large enough to capture a couple of rounds of an announce.
//  I've set the announce time to be 2 seconds, plus up to 12s jitter, so: (12 + 2) * 3 = 42
//  But I'll leave this todo because I don't know if this is a good number.
const PEER_TTL: Duration = Duration::from_secs(42);

/// What a peer has announced, merged per scope. One node can replicate one
/// subject while draining another, each announcing on its own channel, so a
/// scope's entry is refreshed only by an announce that mentions it — and goes
/// stale on its own if the peer stops announcing the scope, even while the
/// peer stays live elsewhere.
struct PeerEntry {
    scopes: VecMap<models::Scope, PeerScope>,
}

/// One scope of a peer's announced state.
struct PeerScope {
    announce: ScopeAnnounce,
    /// Whether the mentioning announce came from a full replica rather than
    /// an offloader merely serving the scope out.
    full_replica: bool,
    last_seen_at: Instant,
}

/// `M` is the metadata type carried by this store's transactions,
/// see [`Transaction`].
pub struct Store<M = ()> {
    /// The manager of things like conflict detection, etc
    db: fjall::OptimisticTxDatabase,
    /// The actual LSM tree that we use to store the data.
    ks: fjall::OptimisticTxKeyspace,

    /// Active remote transactions.
    /// This only tracks _remote_ transactions, not on-going local transactions.
    transactions: CMap<uuid::Uuid, Transaction<M>>,

    /// The most recently received Announce frontier per peer, keyed by the announcing node's id.
    /// Entries older than [`PEER_TTL`] are ignored when computing catch-up state.
    peer_frontiers: CMap<models::NodeId, PeerEntry>,

    replication_handles: CMap<models::Subject, crate::replication::ReplicationHandle>,

    /// Running offloaders, keyed by the scope being drained.
    offload_handles: CMap<models::Scope, crate::replication::ReplicationHandle>,

    clock: Arc<uhlc::HLC>,

    /// When cancelled, all spawned tasks will shut down.
    shutdown: tokio_util::sync::CancellationToken,

    /// Keeping the tmp dir so we can clean it up after we're done.
    _tmp: Option<Arc<tempfile::TempDir>>,
}

impl<M> Clone for Store<M> {
    fn clone(&self) -> Self {
        let Self {
            db,
            ks,
            transactions,
            peer_frontiers,
            replication_handles,
            offload_handles,
            clock,
            shutdown,
            _tmp: tmp,
        } = self;

        Self {
            db: db.clone(),
            ks: ks.clone(),
            transactions: transactions.clone(),
            peer_frontiers: peer_frontiers.clone(),
            replication_handles: replication_handles.clone(),
            offload_handles: offload_handles.clone(),
            clock: clock.clone(),
            shutdown: shutdown.clone(),
            _tmp: tmp.clone(),
        }
    }
}

impl<M: Send + Sync + 'static> Store<M> {
    pub fn init(opts: Options) -> Result<Self> {
        let Options {
            directory,
            logic_clock: lcs,
            gc_interval,
        } = opts;

        let gc_interval = gc_interval.unwrap_or(Duration::from_mins(1));

        let (tmp, directory) = if let Some(directory) = directory {
            (None, directory)
        } else {
            let tmp = tempfile::Builder::new()
                .prefix("db-")
                .tempdir()
                .map_err(|err| {
                    anyhow::anyhow!("Unable to create a temporary directory: {}", err)
                })?;

            let dir = std::path::PathBuf::from(tmp.path());

            (Some(tmp), dir)
        };

        let store = fjall::OptimisticTxDatabase::builder(directory)
            .temporary(tmp.is_some())
            .open()
            .context("unable to open database")?;

        let ks = store
            .keyspace("base", Default::default)
            .context("unable to create keyspace")?;

        let shutdown = tokio_util::sync::CancellationToken::new();

        let store = Self {
            db: store,
            ks,
            clock: lcs,
            transactions: Default::default(),
            peer_frontiers: Default::default(),
            replication_handles: Default::default(),
            offload_handles: Default::default(),
            shutdown,
            _tmp: tmp.map(Arc::new),
        };

        store.check_version_index()?;

        tokio::spawn({
            let store = store.clone();

            async move {
                let shutdown = store.shutdown_token();

                let mut interval = tokio::time::interval(gc_interval);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

                loop {
                    tokio::select! {
                        _ = interval.tick() => (),
                        () = shutdown.cancelled() => break,
                    }

                    // GC scans and deletes synchronously; run it off the async
                    // workers so queryables stay responsive while it works.
                    let gc = tokio::task::spawn_blocking({
                        let store = store.clone();
                        move || store.perform_gc()
                    });

                    match gc.await {
                        Ok(Ok(0)) => (),
                        Ok(Ok(purged)) => {
                            tracing::info!("GC purged {purged} expired sync point(s)");
                        }
                        Ok(Err(err)) => tracing::error!("Unable to perform GC: {}", err),
                        Err(err) => tracing::error!("GC task failed: {}", err),
                    }

                    store.reap_idle_transactions();
                }

                tracing::debug!("GC task shutting down");
            }
        });

        Ok(store)
    }

    /// Returns a token that is cancelled when the store is shut down.
    pub fn shutdown_token(&self) -> tokio_util::sync::CancellationToken {
        self.shutdown.clone()
    }

    #[cfg(test)]
    pub(crate) fn clock(&self) -> Arc<uhlc::HLC> {
        self.clock.clone()
    }

    /// This id _isn't_ stable over restarts.
    pub(crate) fn node_id(&self) -> uhlc::ID {
        *self.clock.get_id()
    }

    /// A version stamped now, comparable with the versions carried by sync
    /// points — the same clock and the same encoding a commit mints from.
    ///
    /// For measuring how old data is by the time it arrives somewhere. That
    /// compares two nodes' readings of a hybrid logical clock, so it is only as
    /// good as the skew between them; useful for telling milliseconds from
    /// seconds, not for anything finer.
    pub fn now(&self) -> models::Version {
        self.clock.new_timestamp().get_time().as_u64()
    }
}

impl<M: Send + Sync + 'static> Store<M> {
    pub fn begin_local(&self, opts: &TransactionOptions) -> Result<Transaction<M>> {
        // fjall technically has a read-only mode, but it's a different type, so we'd have to use something to bridge them.
        // For now, we just hope people don't edit stuff...
        let ts = self.clock.new_timestamp();

        let ks = self.ks.clone();
        let tx = self.db.write_tx()?;

        let tx = Transaction::start(ks, tx, ts, opts);

        Ok(tx)
    }

    pub fn begin_remote(&self, opts: &TransactionOptions) -> Result<TransactionId> {
        let tx = self.begin_local(opts)?;

        let id = uuid::Uuid::now_v7();
        self.transactions.insert(id, tx);

        Ok(id)
    }

    pub fn find_remote_tx(&self, tx_id: TransactionId) -> Option<RemoteTx<'_, M>> {
        let mut tx = self.transactions.get_mut(&tx_id)?;
        tx.touch();
        Some(tx)
    }

    pub fn remove_remote_tx(&self, tx_id: TransactionId) -> Option<Transaction<M>> {
        let (_id, tx) = self.transactions.remove(&tx_id)?;

        Some(tx)
    }

    /// The version of the most recent sync point for `scope`, if any.
    ///
    /// This is the *max* version present, not a gap-free watermark: during
    /// catch-up a later version can be applied while an earlier one is still
    /// pending, so the head can outrun a version that is missing. Use it for
    /// ranking/display, not to decide whether a specific commit is visible —
    /// [`Self::scope_has_version`] answers that.
    pub fn scope_head_version(&self, scope: &models::Scope) -> Result<Option<domain::Version>> {
        let mut tx = self.begin_local(&TransactionOptions::read())?;
        let key = Key::new_scope(&scope.namespace, &scope.database, &scope.schema);

        let head = tx
            .find_last_syncpoint_untracked(key, None)?
            .map(|sp| sp.as_id().1);

        Ok(head)
    }

    /// Whether the sync point for `scope` at exactly `version` is present.
    ///
    /// Unlike a `head >= version` comparison, this is gap-free: it returns true
    /// only if this node has actually applied the commit at `version`, so a
    /// higher version landing first during catch-up cannot make a still-missing
    /// commit read as caught up.
    pub fn scope_has_version(
        &self,
        scope: &models::Scope,
        version: domain::Version,
    ) -> Result<bool> {
        let mut tx = self.begin_local(&TransactionOptions::read())?;
        let key = Key::new_scope(&scope.namespace, &scope.database, &scope.schema);

        Ok(tx
            .find_last_syncpoint_untracked(key, Some(version))?
            .is_some())
    }
}

impl<M: Send + Sync + 'static> Store<M> {
    /// Refuses a database from before the version index existed. Pre-release,
    /// so there is no migration: a fresh database is stamped instead, and an
    /// older one is told to start over.
    pub fn check_version_index(&self) -> Result<()> {
        {
            let tx = self.begin_local(&TransactionOptions::read())?;
            let ready = tx.version_index_ready()?;
            let empty = tx.holds_no_user_data();
            tx.rollback();

            if ready {
                return Ok(());
            }
            if !empty {
                anyhow::bail!(
                    "this database predates the version index and cannot be read; \
                     create a new database (remove the old data directory)"
                );
            }
        }

        let mut tx = self.begin_local(&TransactionOptions::write())?;
        tx.set_version_index_ready();
        tx.commit()
            .context("unable to stamp the version index flag")
    }

    /// Purges retention-expired sync points, returning how many were purged.
    pub fn perform_gc(&self) -> Result<usize> {
        const GC_BATCH: usize = 256;

        let heads = {
            let tx = self.begin_local(&TransactionOptions::read())?;

            let now = tx.timestamp();
            tracing::debug!("Starting GC (now = {})", now.get_time());

            let (lower, upper) = Key::sync_point()
                .range()
                .context("unable to construct sync point range")?;

            let mut heads = vec![];
            tx.collect_latest_heads(lower, upper, |scope, id, sm| {
                let expired = matches!(sm.marker, SyncMarker::Mutation)
                    && matches!(sm.retention_period, Some(rp) if domain::duration_since(id, now) >= rp);

                // Retention expiry is a deletion requirement, honoured even
                // mid-offload: retention travels with each version (symmetric
                // across replicas), so purging before an offload drains never
                // diverges, and keeping expired data alive to offload would only
                // spread copies that must die.
                if expired {
                    heads.push((scope, id, sm));
                }

                Ok(())
            })?;

            tx.rollback();
            heads
        };

        if heads.is_empty() {
            tracing::debug!("Nothing to purge");
            return Ok(0);
        }

        tracing::debug!("found {} items", heads.len());

        // Purge in bounded batches, each its own transaction: progress
        // survives a restart, and concurrent writers aren't starved behind
        // one giant commit.
        let mut purged = 0;
        for batch in heads.chunks(GC_BATCH) {
            let mut tx = self.begin_local(&TransactionOptions::write())?;

            let mut batch_purged = 0;
            for (scope, id, sm) in batch {
                let sp = Key::sync_point()
                    .namespace(&scope.namespace)
                    .database(&scope.database)
                    .schema(&scope.schema)
                    .with_sp_id(*id);

                if let Err(err) = tx.delete_chunk(sp) {
                    tracing::error!("unable to remove expired chunk: {} [skipping...]", err);
                } else {
                    let sp = Key::sync_point()
                        .namespace(&scope.namespace)
                        .database(&scope.database)
                        .schema(&scope.schema)
                        .with_sp_id(*id);
                    tx.mark_syncpoint_purged(&sp, *sm)
                        .context("unable to re-mark a purged sync point")?;
                    batch_purged += 1;
                }
            }

            if let Err(err) = tx.commit() {
                tracing::error!("Unable to remove trash: {} [retrying next cycle]", err);
                return Ok(purged);
            }

            purged += batch_purged;
        }

        Ok(purged)
    }

    /// The sync points this node currently holds for `scope`.
    ///
    /// Taken as a snapshot so a coverage confirmation can be pinned to exactly
    /// what it was about — see [`release_scope`](Self::release_scope).
    pub fn held_sync_points(&self, scope: &models::Scope) -> Result<Vec<models::SyncPointId>> {
        let tx = self.begin_local(&TransactionOptions::read())?;
        let (lower, upper) = Key::sync_point()
            .namespace(&scope.namespace)
            .database(&scope.database)
            .schema(&scope.schema)
            .range()
            .context("unable to construct sync point range for scope")?;

        let mut points = vec![];
        tx.collect_latest_heads(lower, upper, |_scope, id, _sm| {
            points.push(id);
            Ok(())
        })?;

        tx.rollback();

        Ok(points)
    }

    /// Releases the sync points of `scope` a verified holder has been confirmed
    /// to cover. Returns how many were let go.
    ///
    /// `points` is the caller's snapshot of what the confirmation was about,
    /// not "whatever is here now". A commit can land for the scope at any
    /// moment — including in the window between a drain confirming its own
    /// shutdown and getting here, during which the scope reads as neither
    /// replicating nor offloading and a commit will happily start a fresh drain
    /// for it. Releasing by rescanning would forget those rows too, and since
    /// this forgets rather than tombstones, no replica has ever seen them: they
    /// would be gone with nothing to say so.
    ///
    /// The point of offloading a scope is to stop holding it, but the drain
    /// only ever *copied*: `serve_pull` reads rows out and retirement dropped
    /// the locate queryable, so a retired offloader kept every row it had just
    /// handed over. Those copies are not inert. Peers keep vouching for the
    /// node in `peer_view` for `PEER_TTL`, a peek is a `Read` locate that ranks
    /// by head with no state filter, and a stray that just appended is by
    /// construction *ahead* of the replica — so the stale copy wins reads that
    /// the consumer's delete (a `Write`, routed to the replica) can never
    /// reach, and the row is handed to the handler again and again. Measured on
    /// the rack: each command processed 1.5x at tier 2 and up to 5.1x at
    /// tier 3, worst at *low* load, where an idle poll loop has nothing to do
    /// but re-read it.
    ///
    /// Forgets rather than tombstones — see
    /// `Transaction::forget_chunk` for why
    /// asserting a deletion here would tell the real holders to drop the data.
    ///
    /// # Panics
    ///
    /// Never called with a scope this node still needs: the caller must have
    /// confirmed coverage elsewhere first. The other drain exits deliberately
    /// leave the node a replica, and releasing there would drop rows nobody
    /// else holds.
    pub fn release_scope(
        &self,
        scope: &models::Scope,
        points: &[models::SyncPointId],
    ) -> Result<usize> {
        const RELEASE_BATCH: usize = 256;

        if points.is_empty() {
            return Ok(0);
        }

        // Bounded batches, each its own transaction: progress survives a
        // restart and concurrent readers aren't starved behind one big commit.
        let mut released = 0;
        for batch in points.chunks(RELEASE_BATCH) {
            let mut tx = self.begin_local(&TransactionOptions::write())?;

            let mut batch_released = 0;
            for id in batch {
                let sp = Key::sync_point()
                    .namespace(&scope.namespace)
                    .database(&scope.database)
                    .schema(&scope.schema)
                    .with_sp_id(*id);

                if let Err(err) = tx.forget_chunk(sp) {
                    tracing::error!("unable to release an offloaded chunk: {err} [skipping...]");
                } else {
                    batch_released += 1;
                }
            }

            if let Err(err) = tx.commit() {
                tracing::error!("unable to release offloaded data: {err} [{released} released]");
                return Ok(released);
            }

            released += batch_released;
        }

        Ok(released)
    }

    /// Rolls back remote transactions that have outlived their idle timeout.
    pub fn reap_idle_transactions(&self) {
        let now = Instant::now();

        let expired: Vec<uuid::Uuid> = self
            .transactions
            .iter()
            .filter(|entry| entry.value().idle_expired(now))
            .map(|entry| *entry.key())
            .collect();

        for id in expired {
            // Re-check under the removal lock; the tx may have been touched since.
            let Some((_, tx)) = self
                .transactions
                .remove_if(&id, |_, tx| tx.idle_expired(now))
            else {
                continue;
            };

            tracing::warn!("reaping idle transaction {}", id);
            tx.rollback();
        }
    }
}

impl<M: Send + Sync + 'static> Store<M> {
    /// Starts replicating `subject`, returning the replicator to drive.
    ///
    /// `None` means one is already running. Whether this node should take part
    /// at all is the caller's decision; use [`Store::stop_replication`] to
    /// withdraw.
    pub fn replicate<T>(
        &self,
        transport: T,
        subject: models::Subject,
    ) -> Option<crate::replication::Replicator<T, M>>
    where
        T: crate::replication::ReplicaTransport,
    {
        use dashmap::Entry;

        match self.replication_handles.entry(subject.clone()) {
            Entry::Occupied(entry) => {
                if entry.get().is_stopped() {
                    let (replicator, handle) = crate::replication::Replicator::new(
                        self.clone(),
                        transport,
                        subject.clone(),
                        crate::replication::ReplicaMode::Full,
                    );
                    entry.replace_entry(handle);
                    self.spawn_replication_cleanup(subject);
                    Some(replicator)
                } else {
                    None
                }
            }
            Entry::Vacant(entry) => {
                let (replicator, handle) = crate::replication::Replicator::new(
                    self.clone(),
                    transport,
                    subject.clone(),
                    crate::replication::ReplicaMode::Full,
                );
                entry.insert(handle);
                self.spawn_replication_cleanup(subject);
                Some(replicator)
            }
        }
    }

    /// Starts offloading `scope` — announcing and serving it so the nodes
    /// replicating it pull the data off this one — returning the replicator
    /// to drive. The local data is kept, and GC leaves it alone, until a full
    /// replica announces it holds everything we do; only then does the
    /// offloader retire.
    ///
    /// `None` means an offloader is already running, or a replicator covers
    /// the scope (replication already serves everything offloading would).
    pub fn offload<T>(
        &self,
        transport: T,
        scope: models::Scope,
    ) -> Option<crate::replication::Replicator<T, M>>
    where
        T: crate::replication::ReplicaTransport,
    {
        use dashmap::Entry;

        if self.is_replicating(&scope) {
            return None;
        }

        let subject = models::Subject::Scope(scope.clone());

        match self.offload_handles.entry(scope.clone()) {
            Entry::Occupied(entry) => {
                if entry.get().is_stopped() {
                    let (replicator, handle) = crate::replication::Replicator::new(
                        self.clone(),
                        transport,
                        subject,
                        crate::replication::ReplicaMode::Offload,
                    );
                    entry.replace_entry(handle);
                    self.spawn_offload_cleanup(scope);
                    Some(replicator)
                } else {
                    None
                }
            }
            Entry::Vacant(entry) => {
                let (replicator, handle) = crate::replication::Replicator::new(
                    self.clone(),
                    transport,
                    subject,
                    crate::replication::ReplicaMode::Offload,
                );
                entry.insert(handle);
                self.spawn_offload_cleanup(scope);
                Some(replicator)
            }
        }
    }

    /// Whether an offloader is running for `scope`.
    pub fn is_offloading(&self, scope: &models::Scope) -> bool {
        self.offload_handles
            .get(scope)
            .is_some_and(|handle| !handle.is_stopped())
    }

    /// Scopes this node holds sync points for while neither replicating nor
    /// offloading them — data that would otherwise stay stranded here.
    pub fn stray_scopes(&self) -> Result<Vec<models::Scope>> {
        let tx = self.begin_local(&TransactionOptions::read())?;
        let (lower, upper) = Key::sync_point()
            .range()
            .context("unable to construct sync point range")?;

        let mut scopes = std::collections::HashSet::new();
        tx.collect_latest_heads(lower, upper, |scope, _, _| {
            scopes.insert(scope);
            Ok(())
        })?;

        Ok(scopes
            .into_iter()
            .filter(|scope| !self.is_replicating(scope) && !self.is_offloading(scope))
            .collect())
    }

    /// Whether an active replicator's subject covers `scope`.
    pub fn is_replicating(&self, scope: &models::Scope) -> bool {
        self.replication_handles
            .iter()
            .any(|entry| !entry.value().is_stopped() && entry.key().contains(scope))
    }

    /// Stops the replicator for `subject`. A no-op when nothing is
    /// replicating it.
    ///
    /// Data this node still holds is not lost: the scopes turn up in
    /// [`Store::stray_scopes`], and offloading them serves the data out until
    /// a remaining replica verifiably covers it.
    pub fn stop_replication(&self, subject: &models::Subject) {
        if let Some(handle) = self.replication_handles.get(subject) {
            handle.stop();
        }
    }

    fn spawn_replication_cleanup(&self, subject: models::Subject) {
        tokio::spawn({
            let handles = self.replication_handles.clone();

            async move {
                let Some(handle) = handles.get(&subject).map(|h| h.clone()) else {
                    return;
                };
                handle.until_stopped().await;
                // Just gotta make sure the one we're removing _is actually_ the one we should be removing.
                handles.remove_if(&subject, |_, h| h.is_stopped());
            }
        });
    }

    fn spawn_offload_cleanup(&self, scope: models::Scope) {
        tokio::spawn({
            let handles = self.offload_handles.clone();

            async move {
                let Some(handle) = handles.get(&scope).map(|h| h.clone()) else {
                    return;
                };
                handle.until_stopped().await;
                handles.remove_if(&scope, |_, h| h.is_stopped());
            }
        });
    }

    /// Merges an announce's per-scope state into what we know of `peer`.
    /// Scopes the announce doesn't mention keep their previous entries (and
    /// their previous timestamps, so they expire on their own).
    pub(crate) fn record_peer_frontier(
        &self,
        peer: models::NodeId,
        known: VecMap<models::Scope, ScopeAnnounce>,
        full_replica: bool,
    ) {
        let now = Instant::now();
        let mut entry = self
            .peer_frontiers
            .entry(peer)
            .or_insert_with(|| PeerEntry {
                scopes: VecMap::new(),
            });

        for (scope, announce) in known {
            entry.scopes.insert(
                scope,
                PeerScope {
                    announce,
                    full_replica,
                    last_seen_at: now,
                },
            );
        }
    }

    /// The responder's view of who else holds `scope`, for a locate reply.
    ///
    /// Every peer whose announce mentioned the scope within `PEER_TTL`, with
    /// how long ago that was (in this node's clock), its last-known head, and
    /// whether it holds the scope as a replica or a drainer. Lets a single
    /// locate reply surface the whole live set, so a peer too slow to answer
    /// the query itself is still vouched for.
    pub fn peer_view(&self, scope: &models::Scope, now: Instant) -> Vec<models::locate::PeerView> {
        self.peer_frontiers
            .iter()
            .filter_map(|entry| {
                let held = entry.scopes.get(scope)?;
                let age = now.duration_since(held.last_seen_at);
                if age > PEER_TTL {
                    return None;
                }
                let head = held.announce.head()?;
                Some(models::locate::PeerView {
                    id: *entry.key(),
                    age_ms: u64::try_from(age.as_millis()).unwrap_or(u64::MAX),
                    head,
                    state: if held.full_replica {
                        models::locate::HolderState::Replica
                    } else {
                        models::locate::HolderState::Draining
                    },
                })
            })
            .collect()
    }

    /// Aggregates the data from each peer, attempting to build a view of what is remaining.
    ///
    /// Only explicitly announced heads count: gaps below a peer's baseline
    /// show up through `Store::prefix_diverged` instead.
    pub fn outstanding(
        &self,
        scope: &models::Scope,
    ) -> Result<BTreeMap<models::Version, models::Epoch>> {
        let now = Instant::now();
        let mut target: BTreeMap<models::Version, models::Epoch> = BTreeMap::new();

        for entry in self.peer_frontiers.iter() {
            if let Some(held) = entry.scopes.get(scope) {
                if now.duration_since(held.last_seen_at) > PEER_TTL {
                    continue;
                }
                for (&ts, &(epoch, _)) in &held.announce.heads {
                    target
                        .entry(ts)
                        .and_modify(|e| *e = (*e).max(epoch))
                        .or_insert(epoch);
                }
            }
        }

        if target.is_empty() {
            return Ok(target);
        }

        let our_frontier = self.scope_heads(scope)?;

        target.retain(|ts, target_epoch| {
            our_frontier
                .get(ts)
                .is_none_or(|(our_epoch, _)| our_epoch < target_epoch)
        });

        Ok(target)
    }

    fn scope_heads(&self, scope: &models::Scope) -> Result<domain::ScopeFrontier> {
        let tx = self.begin_local(&TransactionOptions::read())?;
        let (lower, upper) = domain::Key::sync_point()
            .namespace(&scope.namespace)
            .database(&scope.database)
            .schema(&scope.schema)
            .range()
            .context("unable to construct sync point range for scope")?;

        let mut our_frontier = domain::ScopeFrontier::new();
        tx.collect_latest_heads(lower, upper, |_scope, id, _| {
            let (epoch, ts, node_id) = id;
            our_frontier.insert(ts, (epoch, node_id));
            Ok(())
        })?;

        Ok(our_frontier)
    }

    /// Whether any live peer's announced baseline fingerprint disagrees with
    /// our own heads below that cut — a divergence the explicit heads in
    /// [`Store::outstanding`] cannot see.
    fn prefix_diverged(&self, scope: &models::Scope) -> Result<bool> {
        let now = Instant::now();
        let mut cuts = vec![];
        for entry in self.peer_frontiers.iter() {
            if let Some(held) = entry.scopes.get(scope)
                && now.duration_since(held.last_seen_at) <= PEER_TTL
                && let Some(cut) = held.announce.baseline
            {
                cuts.push((cut, held.announce.fingerprint));
            }
        }

        if cuts.is_empty() {
            return Ok(false);
        }

        let our_frontier = self.scope_heads(scope)?;

        Ok(cuts.into_iter().any(|(cut, fingerprint)| {
            crate::replication::fold_heads_at_cut(&our_frontier, cut) != fingerprint
        }))
    }

    /// Returns `None` if no live peer has announced anything for this scope
    /// (ie, the scope is untouched by replication so far)
    pub fn replica_status(
        &self,
        scope: &models::Scope,
    ) -> Result<Option<domain::ReplicationStatus>> {
        let now = Instant::now();
        let any_live_peer_mentions = self.peer_frontiers.iter().any(|entry| {
            entry
                .scopes
                .get(scope)
                .is_some_and(|held| now.duration_since(held.last_seen_at) <= PEER_TTL)
        });
        if !any_live_peer_mentions {
            return Ok(None);
        }

        let caught_up = self.outstanding(scope)?.is_empty() && !self.prefix_diverged(scope)?;

        Ok(Some(if caught_up {
            domain::ReplicationStatus::Active
        } else {
            domain::ReplicationStatus::Requested
        }))
    }
}
