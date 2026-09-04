use db_commons::models::ReplicaMessage;
use db_commons::models::replication::{ScopeAnnounce, ScopeFrontier, VecMap};

use crate::domain;

use crate::domain::{ReplicationStatus, api};
use crate::store::TransactionOptions;
use crate::store::fjall::Store;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone)]
struct CaptureTransport {
    id: uhlc::ID,
    outgoing: Arc<Mutex<Vec<ReplicaMessage>>>,
}

impl CaptureTransport {
    pub fn new() -> Self {
        let id = uhlc::ID::rand();
        Self {
            id,
            outgoing: Default::default(),
        }
    }

    pub fn len(&self) -> usize {
        let messages = self.outgoing.lock().expect("unable to lock outgoing");
        messages.len()
    }

    pub fn drain_outgoing(&self) -> Vec<ReplicaMessage> {
        let mut messages = self.outgoing.lock().expect("unable to lock outgoing");
        messages.drain(..).collect::<Vec<_>>()
    }
}

impl crate::replication::ReplicaTransport for CaptureTransport {
    fn publish(&self, msg: ReplicaMessage) -> impl Future<Output = ()> + Send {
        {
            let mut messages = self.outgoing.lock().expect("unable to lock outgoing");
            messages.push(msg);
        }
        std::future::ready(())
    }
}

fn db_scope(scope: &api::Scope) -> domain::Scope<'_> {
    domain::Key::new_scope(&scope.namespace, &scope.database, &scope.schema)
}

fn spawn_replica(
    subject: &domain::Subject,
) -> (
    Store,
    CaptureTransport,
    crate::replication::Replicator<CaptureTransport>,
) {
    let store = super::open_tmp();
    let transport = CaptureTransport::new();
    let handle = store.replicate(transport.clone(), subject.clone()).unwrap();

    (store, transport, handle)
}

async fn exchange(
    replica1: &crate::replication::Replicator<CaptureTransport>,
    transport1: &CaptureTransport,
    replica2: &crate::replication::Replicator<CaptureTransport>,
    transport2: &CaptureTransport,
) {
    let msg1 = transport1.drain_outgoing();
    let msg2 = transport2.drain_outgoing();

    for msg2 in msg2 {
        replica1.clone().handle_message(transport2.id, msg2).await;
    }
    for msg1 in msg1 {
        replica2.clone().handle_message(transport1.id, msg1).await;
    }
}

/// Drive announce + repeated exchange until both replicas stop talking.
async fn settle(
    replica1: &crate::replication::Replicator<CaptureTransport>,
    transport1: &CaptureTransport,
    replica2: &crate::replication::Replicator<CaptureTransport>,
    transport2: &CaptureTransport,
) {
    replica1.announce().await.expect("unable to announce");
    replica2.announce().await.expect("unable to announce");

    for _ in 0..8 {
        if transport1.len() == 0 && transport2.len() == 0 {
            break;
        }
        exchange(replica1, transport1, replica2, transport2).await;
    }
}

fn add_key(store: &Store, scope: domain::Scope<'_>, key: &str, value: &[u8]) {
    let mut tx = store
        .begin_local(&TransactionOptions::write())
        .expect("unable to start write tx");
    tx.key_put(scope.kv(key), value).expect("unable to read db");
    tx.commit().expect("unable to commit");
}

fn get(store: &Store, scope: domain::Scope<'_>, key: &str) -> Option<Vec<u8>> {
    let mut tx = store
        .begin_local(&TransactionOptions::read())
        .expect("unable to start read tx");
    tx.key_get(scope.kv(key)).expect("unable to read key")
}

fn add_key_with_retention(
    store: &Store,
    scope: domain::Scope<'_>,
    key: &str,
    value: &[u8],
    retention: std::time::Duration,
) {
    let mut tx = store
        .begin_local(&TransactionOptions::retain_for(
            crate::store::TransactionMode::ReadWrite,
            retention,
        ))
        .expect("unable to start write tx");
    tx.key_put(scope.kv(key), value).expect("unable to put");
    tx.commit().expect("unable to commit");
}

/// Insert then delete the same key in one tx; reads back absent (delete wins).
fn add_then_delete_key(store: &Store, scope: domain::Scope<'_>, key: &str, value: &[u8]) {
    let mut tx = store
        .begin_local(&TransactionOptions::write())
        .expect("unable to start write tx");
    tx.key_put(scope.kv(key), value).expect("unable to put");
    tx.key_delete(scope.kv(key)).expect("unable to delete");
    tx.commit().expect("unable to commit");
}

#[tokio::test]
#[rustfmt::skip]
async fn restore_propagates_to_peers() {
    let subject = domain::Subject::Namespace("d".to_string());

    let (store1, transport1, replica1) = spawn_replica(&subject);
    let (store2, transport2, replica2) = spawn_replica(&subject);

    eprintln!("A is {}", store1.node_id());
    eprintln!("B is {}", store2.node_id());

    let scope = api::Scope::new("d", "db", "schema");
    let dscope = db_scope(&scope);

    // changeset 1: key "a"
    add_key(&store1, dscope, "a", b"1");

    // Snapshot store1 while it only contains "a".
    let early = {
        let tx = store1
            .begin_local(&TransactionOptions::read())
            .expect("unable to start read tx");
        tx.take_snapshot(dscope).expect("unable to take snapshot")
    };

    // changeset 2: key "b"
    add_key(&store1, dscope, "b", b"2");

    // Replicate fully — store2 should now hold both keys.
    settle(&replica1, &transport1, &replica2, &transport2).await;
    assert_eq!(
        get(&store2, dscope, "b").as_deref(),
        Some(&b"2"[..]),
        "precondition: replication should have delivered b to store2",
    );

    // Authoritatively restore the early snapshot on store1, dropping "b".
    {
        let mut tx = store1
            .begin_local(&TransactionOptions::write())
            .expect("unable to start write tx");
        tx.restore_snapshot(dscope, early)
            .expect("unable to restore snapshot");
        tx.commit().expect("unable to commit restore");
    }
    assert_eq!(
        get(&store1, dscope, "b"),
        None,
        "restore should have removed b locally",
    );

    // Run replication again. The cluster must converge to the restored state.
    settle(&replica1, &transport1, &replica2, &transport2).await;

    let a_on_1 = get(&store1, dscope, "a");
    let a_on_2 = get(&store2, dscope, "a");
    let b_on_1 = get(&store1, dscope, "b");
    let b_on_2 = get(&store2, dscope, "b");
    eprintln!("after re-replication: a_on_1={a_on_1:?} a_on_2={a_on_2:?} b_on_1={b_on_1:?} b_on_2={b_on_2:?}");

    // The snapshot's data survives everywhere.
    assert_eq!(a_on_1.as_deref(), Some(&b"1"[..]), "a should remain on store1");
    assert_eq!(a_on_2.as_deref(), Some(&b"1"[..]), "a should remain on store2");

    // The dropped data is gone everywhere: not re-pushed onto store1...
    assert_eq!(b_on_1, None, "restore must not be reverted by the peer");
    // ...and the removal propagated to the peer that still held it.
    assert_eq!(b_on_2, None, "removal must propagate to store2");
}

/// A key updated across versions must survive a restore to an earlier snapshot:
/// the snapshot still holds the key (at its older value), so it must not be
/// dropped just because the version that later touched it is gone.
#[tokio::test]
#[rustfmt::skip]
async fn restore_keeps_keys_updated_in_dropped_versions() {
    let subject = domain::Subject::Namespace("d".to_string());

    let (store1, transport1, replica1) = spawn_replica(&subject);
    let (store2, transport2, replica2) = spawn_replica(&subject);

    let scope = api::Scope::new("d", "db", "schema");
    let dscope = db_scope(&scope);

    // changeset 1: x = A
    add_key(&store1, dscope, "x", b"A");

    // Snapshot while the state is { x: A }.
    let early = {
        let tx = store1
            .begin_local(&TransactionOptions::read())
            .expect("unable to start read tx");
        tx.take_snapshot(dscope).expect("unable to take snapshot")
    };

    // changeset 2: update x = B and add y = C (a separate, later version).
    add_key(&store1, dscope, "x", b"B");
    add_key(&store1, dscope, "y", b"C");

    settle(&replica1, &transport1, &replica2, &transport2).await;
    assert_eq!(get(&store2, dscope, "y").as_deref(), Some(&b"C"[..]));

    // Restore the early snapshot, then re-replicate.
    {
        let mut tx = store1
            .begin_local(&TransactionOptions::write())
            .expect("unable to start write tx");
        tx.restore_snapshot(dscope, early)
            .expect("unable to restore snapshot");
        tx.commit().expect("unable to commit restore");
    }
    settle(&replica1, &transport1, &replica2, &transport2).await;

    // x survives everywhere at its snapshot value (it's in the snapshot, even
    // though the version that later updated it was dropped).
    assert_eq!(get(&store1, dscope, "x").as_deref(), Some(&b"A"[..]), "x on store1");
    assert_eq!(get(&store2, dscope, "x").as_deref(), Some(&b"A"[..]), "x on store2");
    // y was only ever in the dropped version, so it's gone everywhere.
    assert_eq!(get(&store1, dscope, "y"), None, "y on store1");
    assert_eq!(get(&store2, dscope, "y"), None, "y on store2");
}

/// A key inserted then deleted in one tx must replicate as deleted, not
/// resurrected (the version carries both markers; the changeset must dedup).
#[tokio::test]
#[rustfmt::skip]
async fn insert_then_delete_in_one_tx_replicates_as_deleted() {
    let subject = domain::Subject::Namespace("d".to_string());

    let (store1, transport1, replica1) = spawn_replica(&subject);
    let (store2, transport2, replica2) = spawn_replica(&subject);

    let scope = api::Scope::new("d", "db", "schema");
    let dscope = db_scope(&scope);

    // Insert then delete "k" in a single transaction on store1.
    add_then_delete_key(&store1, dscope, "k", b"value");
    assert_eq!(get(&store1, dscope, "k"), None, "precondition: deletion wins locally");

    // Replicate. The peer must converge to the same (absent) state.
    settle(&replica1, &transport1, &replica2, &transport2).await;

    assert_eq!(
        get(&store2, dscope, "k"),
        None,
        "peer must not resurrect a key deleted in the same tx it was inserted",
    );
}

/// During catch-up, changesets are applied as independent commits and can land
/// out of order, so a later version can be present while an earlier one is still
/// pending. `scope_head_version` returns the *max* present version, which then
/// outruns the missing one — it is not a gap-free "caught up" watermark.
/// `scope_has_version` answers the exact-presence question the routing checks
/// actually need: "have I applied *this* commit?".
#[tokio::test]
async fn scope_has_version_detects_a_gap_below_head() {
    let subject = domain::Subject::Namespace("d".to_string());

    let (store1, transport1, replica1) = spawn_replica(&subject);
    let (store2, transport2, replica2) = spawn_replica(&subject);

    let scope = api::Scope::new("d", "db", "schema");
    let dscope = db_scope(&scope);

    // Two independent commits on store1 → two sync points at distinct versions.
    add_key(&store1, dscope, "a", b"1");
    add_key(&store1, dscope, "b", b"2");

    // Learn the two versions store1 produced, oldest first.
    let mut versions = vec![];
    {
        let tx = store1
            .begin_local(&TransactionOptions::read())
            .expect("unable to start read tx");
        let (lower, upper) = domain::SyncPoint::range_from_subject(&subject).expect("sp range");
        tx.collect_latest_heads(lower, upper, |s, id, _| {
            if s == scope {
                versions.push(id.1);
            }
            Ok(())
        })
        .expect("unable to collect heads");
    }
    versions.sort_unstable();
    assert_eq!(versions.len(), 2, "two commits should yield two versions");
    let (early, late) = (versions[0], versions[1]);

    // Drive catch-up but deliver ONLY the later changeset to store2, leaving the
    // earlier version pending — exactly the out-of-order window.
    replica1.announce().await.expect("unable to announce");
    for msg in transport1.drain_outgoing() {
        replica2.clone().handle_message(transport1.id, msg).await;
    }
    for msg in transport2.drain_outgoing() {
        replica1.clone().handle_message(transport2.id, msg).await;
    }

    let changesets = transport1.drain_outgoing();
    for msg in changesets {
        if let ReplicaMessage::ChangeSet(mut cs) = msg {
            assert_eq!(
                cs.chunks.len(),
                2,
                "both sync points batch into one changeset"
            );
            // Withhold `early`: keep only the `late` chunk, leaving store2 a gap.
            cs.chunks.retain(|chunk| chunk.id.1 == late);
            replica2
                .clone()
                .handle_message(transport1.id, ReplicaMessage::ChangeSet(cs))
                .await;
        }
    }

    // The head is the max present version, so it reports `late` despite the gap.
    assert_eq!(
        store2.scope_head_version(&scope).expect("head"),
        Some(late),
        "head is the max present version, even with an earlier gap",
    );

    // Exact presence distinguishes the two: `late` applied, `early` still missing.
    assert!(
        store2.scope_has_version(&scope, late).expect("has late"),
        "the delivered version is present",
    );
    assert!(
        !store2.scope_has_version(&scope, early).expect("has early"),
        "the withheld version must not read as present just because head passed it",
    );
}

#[tokio::test]
async fn changesets_batch_multiple_sync_points() {
    use db_commons::models::replication::ChangeSetReq;

    let subject = domain::Subject::Namespace("d".to_string());
    let (store, transport, replica) = spawn_replica(&subject);
    let scope = api::Scope::new("d", "db", "schema");

    // Many small commits produce many distinct sync points.
    for i in 0..50u32 {
        add_key(&store, db_scope(&scope), &format!("k{i}"), b"v");
    }

    // A from-scratch request must serve every sync point.
    replica
        .clone()
        .handle_message(
            uhlc::ID::rand(),
            ReplicaMessage::ChangeSetReq(ChangeSetReq {
                tx_id: None,
                scope: scope.clone(),
                since_ts: None,
                epoch_floors: std::collections::BTreeMap::new(),
            }),
        )
        .await;

    let changesets: Vec<_> = transport
        .drain_outgoing()
        .into_iter()
        .filter_map(|m| match m {
            ReplicaMessage::ChangeSet(cs) => Some(cs),
            _ => None,
        })
        .collect();

    let total_chunks: usize = changesets.iter().map(|cs| cs.chunks.len()).sum();
    assert_eq!(total_chunks, 50, "every sync point is served exactly once");
    assert_eq!(
        changesets.len(),
        1,
        "a catch-up is served as a single changeset",
    );
}

fn spawn_offloader(
    scope: &api::Scope,
) -> (
    Store,
    CaptureTransport,
    crate::replication::Replicator<CaptureTransport>,
) {
    let store = super::open_tmp();
    let transport = CaptureTransport::new();
    let handle = store.offload(transport.clone(), scope.clone()).unwrap();

    (store, transport, handle)
}

#[tokio::test]
async fn an_offloader_drains_a_stray_scope_to_a_replica() {
    let scope = api::Scope::new("d", "db", "schema");
    let (store1, transport1, offloader) = spawn_offloader(&scope);

    let subject = domain::Subject::Namespace("d".to_string());
    let (store2, transport2, replica) = spawn_replica(&subject);

    add_key(&store1, db_scope(&scope), "a", b"1");

    settle(&offloader, &transport1, &replica, &transport2).await;

    assert_eq!(
        get(&store2, db_scope(&scope), "a").as_deref(),
        Some(&b"1"[..]),
        "the replica should have pulled the stray data off the offloader",
    );
}

#[tokio::test]
async fn an_offloader_never_pulls_data() {
    use db_commons::models::replication::ChangeSetReq;

    let scope = api::Scope::new("d", "db", "schema");
    let (store1, transport1, offloader) = spawn_offloader(&scope);

    let subject = domain::Subject::Namespace("d".to_string());
    let (store2, transport2, replica) = spawn_replica(&subject);

    add_key(&store2, db_scope(&scope), "b", b"2");

    // The replica announces data the offloader lacks; it must not request it.
    replica.announce().await.expect("unable to announce");
    for msg in transport2.drain_outgoing() {
        offloader.clone().handle_message(transport2.id, msg).await;
    }
    assert!(
        transport1.drain_outgoing().is_empty(),
        "an offloader must not request changesets",
    );

    // Nor apply a changeset pushed at it directly.
    let req = ReplicaMessage::ChangeSetReq(ChangeSetReq {
        tx_id: None,
        scope: scope.clone(),
        since_ts: None,
        epoch_floors: Default::default(),
    });
    replica.clone().handle_message(transport1.id, req).await;

    let changesets = transport2.drain_outgoing();
    assert!(
        !changesets.is_empty(),
        "the replica should serve the request"
    );
    for msg in changesets {
        offloader.clone().handle_message(transport2.id, msg).await;
    }

    assert_eq!(
        get(&store1, db_scope(&scope), "b"),
        None,
        "an offloader must not apply changesets",
    );
}

#[tokio::test]
async fn an_offloader_retires_once_a_replica_covers_it() {
    let scope = api::Scope::new("d", "db", "schema");
    let (store1, transport1, offloader) = spawn_offloader(&scope);

    let subject = domain::Subject::Namespace("d".to_string());
    let (store2, transport2, replica) = spawn_replica(&subject);

    add_key(&store1, db_scope(&scope), "a", b"1");

    // An announce from a peer holding nothing must not retire the offloader.
    replica.announce().await.expect("unable to announce");
    for msg in transport2.drain_outgoing() {
        offloader.clone().handle_message(transport2.id, msg).await;
    }
    transport1.drain_outgoing();
    assert!(
        store1.is_offloading(&scope),
        "uncovered data keeps the offloader alive",
    );

    // Let the replica pull everything, then announce what it now holds.
    settle(&offloader, &transport1, &replica, &transport2).await;
    assert_eq!(
        get(&store2, db_scope(&scope), "a").as_deref(),
        Some(&b"1"[..])
    );

    replica.announce().await.expect("unable to announce");
    for msg in transport2.drain_outgoing() {
        offloader.clone().handle_message(transport2.id, msg).await;
    }

    assert!(
        !store1.is_offloading(&scope),
        "a covering announce retires the offloader",
    );
}

/// The point of offloading is to stop holding the scope — but letting go must
/// not be expressible as a deletion, or it would take the holder's copy too.
#[tokio::test]
async fn releasing_an_offloaded_scope_leaves_the_holders_copy_alone() {
    let scope = api::Scope::new("d", "db", "schema");
    let (store1, transport1, offloader) = spawn_offloader(&scope);

    let subject = domain::Subject::Namespace("d".to_string());
    let (store2, transport2, replica) = spawn_replica(&subject);

    add_key(&store1, db_scope(&scope), "a", b"1");

    // The replica pulls everything the offloader holds, which is what a
    // coverage-verified retirement establishes before releasing.
    settle(&offloader, &transport1, &replica, &transport2).await;
    assert_eq!(
        get(&store2, db_scope(&scope), "a").as_deref(),
        Some(&b"1"[..]),
        "the holder must have the data before the offloader may drop it",
    );

    let covered = store1
        .held_sync_points(&scope)
        .expect("unable to snapshot the held sync points");
    let released = store1
        .release_scope(&scope, &covered)
        .expect("unable to release");
    assert!(released > 0, "there were sync points to release");

    assert_eq!(
        get(&store1, db_scope(&scope), "a"),
        None,
        "a released scope is no longer served from here — the stale copy \
         answering reads is what made every command arrive more than once",
    );

    // The property that matters. A `SyncMarker::Deletion` marker replicates as
    // an instruction to erase the version (`insert_changeset`), so if releasing
    // left one behind, another round of gossip would delete the holder's copy
    // as well — losing the only remaining data.
    settle(&offloader, &transport1, &replica, &transport2).await;
    replica.announce().await.expect("unable to announce");
    for msg in transport2.drain_outgoing() {
        offloader.clone().handle_message(transport2.id, msg).await;
    }
    for msg in transport1.drain_outgoing() {
        replica.clone().handle_message(transport1.id, msg).await;
    }

    assert_eq!(
        get(&store2, db_scope(&scope), "a").as_deref(),
        Some(&b"1"[..]),
        "releasing locally must never propagate as a deletion",
    );
}

/// Releasing drops what a holder was confirmed to cover, not whatever is here
/// by the time the release runs.
///
/// `confirm_shutdown` makes the scope read as neither replicating nor
/// offloading immediately, so a commit landing in the window before the release
/// happily starts a fresh drain and writes rows no replica has ever seen. A
/// release that rescanned would forget those too — and since this forgets
/// rather than tombstones, they would be gone with nothing anywhere to say so.
#[tokio::test]
async fn releasing_leaves_rows_written_after_the_coverage_check() {
    let scope = api::Scope::new("d", "db", "schema");
    let (store1, transport1, offloader) = spawn_offloader(&scope);

    let subject = domain::Subject::Namespace("d".to_string());
    let (store2, transport2, replica) = spawn_replica(&subject);

    add_key(&store1, db_scope(&scope), "covered", b"1");

    settle(&offloader, &transport1, &replica, &transport2).await;
    assert_eq!(
        get(&store2, db_scope(&scope), "covered").as_deref(),
        Some(&b"1"[..]),
        "the holder must have the data before the offloader may drop it",
    );

    // What the coverage confirmation was about, snapshotted at the moment of
    // the check — exactly as the drain loop takes it.
    let covered = store1
        .held_sync_points(&scope)
        .expect("unable to snapshot the held sync points");

    // The racing commit: it lands after the check and before the release, and
    // nothing has replicated it anywhere.
    add_key(&store1, db_scope(&scope), "raced", b"2");

    let released = store1
        .release_scope(&scope, &covered)
        .expect("unable to release");
    assert_eq!(
        released,
        covered.len(),
        "exactly the confirmed sync points go",
    );

    assert_eq!(
        get(&store1, db_scope(&scope), "covered"),
        None,
        "the confirmed rows are released",
    );
    assert_eq!(
        get(&store1, db_scope(&scope), "raced").as_deref(),
        Some(&b"2"[..]),
        "a row nobody was confirmed to hold must survive the release",
    );
}

#[tokio::test]
async fn an_offloader_does_not_retire_off_another_offloader() {
    let scope = api::Scope::new("d", "db", "schema");
    let (store1, transport1, offloader) = spawn_offloader(&scope);

    add_key(&store1, db_scope(&scope), "a", b"1");

    // The announce this offloader emits is exactly what a second offloader
    // holding the same data would send: it covers our frontier, but comes from
    // a node that merely serves the data out rather than durably retaining it.
    offloader.announce().await.expect("unable to announce");
    let peer_announce = transport1.drain_outgoing();
    assert!(!peer_announce.is_empty(), "the offloader should announce");

    let peer = uhlc::ID::rand();
    for msg in peer_announce {
        offloader.clone().handle_message(peer, msg).await;
    }

    assert!(
        store1.is_offloading(&scope),
        "an offloader must not retire off another offloader's coverage",
    );
}

#[tokio::test]
async fn gc_reclaims_expired_data_even_while_offloading() {
    let scope = api::Scope::new("d", "db", "schema");
    let (store, _transport, _offloader) = spawn_offloader(&scope);

    // Retention has already elapsed. Expiry is a deletion requirement, so GC
    // reclaims it even though the scope is still being offloaded.
    add_key_with_retention(&store, db_scope(&scope), "a", b"1", Duration::ZERO);
    assert!(store.is_offloading(&scope));

    store.perform_gc().expect("gc failed");

    assert_eq!(
        get(&store, db_scope(&scope), "a"),
        None,
        "expired data is reclaimed even while the scope is offloading",
    );
}

#[tokio::test]
async fn gc_keeps_unexpired_data_while_offloading() {
    let scope = api::Scope::new("d", "db", "schema");
    let (store, _transport, _offloader) = spawn_offloader(&scope);

    // A long retention → not expired; GC keeps it regardless of offload state.
    add_key_with_retention(&store, db_scope(&scope), "a", b"1", Duration::from_hours(1));
    assert!(store.is_offloading(&scope));

    store.perform_gc().expect("gc failed");

    assert_eq!(
        get(&store, db_scope(&scope), "a").as_deref(),
        Some(&b"1"[..]),
        "only expired data is reclaimed — unexpired data survives GC",
    );
}

#[tokio::test]
async fn gc_reclaims_a_scope_once_offloading_stops() {
    let scope = api::Scope::new("d", "db", "schema");
    let (store, _transport, offloader) = spawn_offloader(&scope);

    add_key_with_retention(&store, db_scope(&scope), "a", b"1", Duration::ZERO);

    offloader.confirm_shutdown();
    assert!(!store.is_offloading(&scope));

    store.perform_gc().expect("gc failed");

    assert_eq!(
        get(&store, db_scope(&scope), "a"),
        None,
        "a retired scope's expired data is reclaimed as usual",
    );
}

#[tokio::test]
async fn gc_purges_an_expired_syncpoint_only_once() {
    let store = super::open_tmp();
    let scope = api::Scope::new("d", "db", "schema");

    add_key_with_retention(&store, db_scope(&scope), "a", b"1", Duration::ZERO);

    let purged = store.perform_gc().expect("gc failed");
    assert_eq!(purged, 1, "the expired sync point is purged");

    let purged = store.perform_gc().expect("gc failed");
    assert_eq!(purged, 0, "a purged sync point is never re-collected");
}

#[tokio::test]
async fn gc_flips_purged_syncpoints_to_deletion_markers() {
    let store = super::open_tmp();
    let scope = api::Scope::new("d", "db", "schema");

    add_key_with_retention(&store, db_scope(&scope), "a", b"1", Duration::ZERO);

    store.perform_gc().expect("gc failed");

    let tx = store
        .begin_local(&TransactionOptions::read())
        .expect("unable to start read tx");
    let chunks = tx.take_snapshot(db_scope(&scope)).expect("snapshot failed");

    assert!(
        !chunks.is_empty(),
        "the purged sync point itself is retained"
    );
    assert!(
        chunks
            .iter()
            .all(|c| matches!(c.meta.marker, domain::SyncMarker::Deletion)),
        "a purged sync point is re-marked Deletion so GC never re-collects it: {:?}",
        chunks.iter().map(|c| c.meta.marker).collect::<Vec<_>>(),
    );
}

/// A transport whose sync queries are answered directly by a peer replicator,
/// standing in for the zenoh queryable.
#[derive(Clone)]
struct PullTransport {
    inner: CaptureTransport,
    peer: Arc<Mutex<Option<crate::replication::Replicator<CaptureTransport>>>>,
}

impl PullTransport {
    fn new() -> Self {
        Self {
            inner: CaptureTransport::new(),
            peer: Arc::default(),
        }
    }
}

impl crate::replication::ReplicaTransport for PullTransport {
    fn publish(&self, msg: ReplicaMessage) -> impl Future<Output = ()> + Send {
        self.inner.publish(msg)
    }

    fn can_sync(&self) -> bool {
        true
    }

    async fn pull(
        &self,
        _target: uhlc::ID,
        req: db_commons::models::replication::sync::PullRequest,
    ) -> Option<db_commons::models::replication::sync::PullResponse> {
        let peer = self.peer.lock().expect("peer poisoned").clone()?;
        peer.serve_pull(&req, usize::MAX).ok()
    }

    async fn verify(
        &self,
        _target: uhlc::ID,
        req: db_commons::models::replication::sync::VerifyRequest,
    ) -> Option<bool> {
        let peer = self.peer.lock().expect("peer poisoned").clone()?;
        peer.verify_coverage(&req).ok()
    }
}

/// A replica that falls behind a non-full-replica holder (a drain) pulls from
/// it directly instead of broadcasting a changeset request.
#[tokio::test]
async fn a_replica_pulls_directly_from_a_drain() {
    let subject = domain::Subject::Namespace("d".to_string());
    let scope = api::Scope::new("d", "db", "schema");

    // The drain: an offloader holding one key.
    let (drain_store, drain_transport, drain) = spawn_offloader(&scope);
    add_key(&drain_store, db_scope(&scope), "a", b"1");

    // The replica, whose sync queries land on the drain.
    let store_b = super::open_tmp();
    let transport_b = PullTransport::new();
    let replica_b = store_b
        .replicate(transport_b.clone(), subject.clone())
        .expect("unable to replicate");
    *transport_b.peer.lock().expect("peer poisoned") = Some(drain.clone());

    drain.announce().await.expect("announce failed");
    let announce = drain_transport
        .drain_outgoing()
        .into_iter()
        .find_map(|msg| match msg {
            ReplicaMessage::Announce(a) => Some(a),
            _ => None,
        })
        .expect("the drain announced");

    replica_b
        .clone()
        .handle_message(drain_transport.id, ReplicaMessage::Announce(announce))
        .await;

    assert_eq!(
        get(&store_b, db_scope(&scope), "a").as_deref(),
        Some(&b"1"[..]),
        "the replica pulled the drain's data directly",
    );
    let broadcast_reqs = transport_b
        .inner
        .drain_outgoing()
        .into_iter()
        .filter(|msg| matches!(msg, ReplicaMessage::ChangeSetReq(_)))
        .count();
    assert_eq!(
        broadcast_reqs, 0,
        "no broadcast changeset request was needed"
    );
}

/// When a pull from a drain fails, a sync-capable replica waits for the
/// drain's next announce instead of falling back to broadcast gossip: the
/// changeset broadcast amplifies mesh-wide, while the announce retry is
/// cheap and the drain isn't going anywhere.
#[tokio::test]
async fn a_failed_pull_from_a_drain_does_not_fall_back_to_gossip() {
    let subject = domain::Subject::Namespace("d".to_string());
    let scope = api::Scope::new("d", "db", "schema");

    let (drain_store, drain_transport, drain) = spawn_offloader(&scope);
    add_key(&drain_store, db_scope(&scope), "a", b"1");

    // A sync-capable replica whose pulls fail (no peer wired up).
    let store_b = super::open_tmp();
    let transport_b = PullTransport::new();
    let replica_b = store_b
        .replicate(transport_b.clone(), subject.clone())
        .expect("unable to replicate");

    drain.announce().await.expect("announce failed");
    let announce = drain_transport
        .drain_outgoing()
        .into_iter()
        .find_map(|msg| match msg {
            ReplicaMessage::Announce(a) => Some(a),
            _ => None,
        })
        .expect("the drain announced");
    replica_b
        .clone()
        .handle_message(drain_transport.id, ReplicaMessage::Announce(announce))
        .await;

    let fallbacks = transport_b
        .inner
        .drain_outgoing()
        .into_iter()
        .filter(|msg| {
            matches!(
                msg,
                ReplicaMessage::ChangeSetReq(_) | ReplicaMessage::Probe(_)
            )
        })
        .count();
    assert_eq!(
        fallbacks, 0,
        "a failed pull retries on the drain's next announce, not over gossip",
    );
    assert!(
        get(&store_b, db_scope(&scope), "a").is_none(),
        "nothing arrived — the failed pull must not be papered over by gossip",
    );
}

/// A replica can pull a holder's sync points directly, page by page, until
/// its copy of the scope matches the holder's.
#[tokio::test]
async fn direct_pull_pages_until_caught_up() {
    use db_commons::models::replication::sync;

    let subject = domain::Subject::Namespace("d".to_string());
    let (store_a, _ta, replica_a) = spawn_replica(&subject);
    let (store_b, _tb, replica_b) = spawn_replica(&subject);

    let scope = api::Scope::new("d", "db", "schema");
    for i in 0..5 {
        add_key(&store_a, db_scope(&scope), &format!("k{i}"), b"v");
    }

    let mut req = sync::PullRequest {
        scope: scope.clone(),
        after: None,
        since_ts: None,
        epoch_floors: Default::default(),
    };

    let mut pages = 0;
    loop {
        let sync::PullResponse { chunks, next } =
            replica_a.serve_pull(&req, 64).expect("serve failed");
        replica_b
            .apply_pull(&scope, chunks)
            .await
            .expect("apply failed");
        pages += 1;

        match next {
            Some(cursor) => req.after = Some(cursor),
            None => break,
        }
    }
    assert!(pages > 1, "a tiny page budget forces paging: {pages}");

    let heads = |store: &Store| {
        let tx = store
            .begin_local(&TransactionOptions::read())
            .expect("unable to start read tx");
        tx.take_snapshot(db_scope(&scope))
            .expect("snapshot failed")
            .into_iter()
            .map(|c| c.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        heads(&store_a),
        heads(&store_b),
        "the pull mirrors the holder"
    );
}

/// A pull with a `since_ts` watermark serves only what the requester lacks.
#[tokio::test]
async fn direct_pull_serves_only_whats_missing() {
    use db_commons::models::replication::sync;

    let subject = domain::Subject::Namespace("d".to_string());
    let (store_a, _ta, replica_a) = spawn_replica(&subject);

    let scope = api::Scope::new("d", "db", "schema");
    add_key(&store_a, db_scope(&scope), "old", b"1");

    let head = {
        let tx = store_a
            .begin_local(&TransactionOptions::read())
            .expect("unable to start read tx");
        let chunks = tx.take_snapshot(db_scope(&scope)).expect("snapshot failed");
        chunks.last().expect("a sync point exists").id.1
    };

    add_key(&store_a, db_scope(&scope), "new", b"2");

    let req = sync::PullRequest {
        scope: scope.clone(),
        after: None,
        since_ts: Some(head),
        epoch_floors: Default::default(),
    };
    let sync::PullResponse { chunks, next } = replica_a
        .serve_pull(&req, usize::MAX)
        .expect("serve failed");

    assert_eq!(
        chunks.len(),
        1,
        "only the commit past the watermark is served"
    );
    assert!(next.is_none());
}

/// Coverage: a holder confirms it covers a set of heads only when it holds
/// every one of them at the same or a newer epoch.
#[tokio::test]
async fn coverage_verify_requires_every_head() {
    use db_commons::models::replication::sync;

    let subject = domain::Subject::Namespace("d".to_string());
    let (store_a, _ta, replica_a) = spawn_replica(&subject);

    let scope = api::Scope::new("d", "db", "schema");
    add_key(&store_a, db_scope(&scope), "a", b"1");

    let (epoch, ts, _node) = {
        let tx = store_a
            .begin_local(&TransactionOptions::read())
            .expect("unable to start read tx");
        let chunks = tx.take_snapshot(db_scope(&scope)).expect("snapshot failed");
        chunks[0].id
    };

    let covered = |heads: Vec<(u64, u64)>| {
        replica_a
            .verify_coverage(&sync::VerifyRequest {
                scope: scope.clone(),
                heads,
            })
            .expect("verify failed")
    };

    assert!(covered(vec![(ts, epoch)]), "an equal epoch is covered");
    assert!(
        !covered(vec![(ts, epoch + 1)]),
        "a newer epoch than held is not covered",
    );
    assert!(
        !covered(vec![(ts + 1, epoch)]),
        "an unknown version is not covered",
    );
    assert!(
        !covered(vec![(ts, epoch), (ts + 1, epoch)]),
        "one missing head fails the whole page",
    );
}

/// A partial (parented) restore must only touch the restored scope: its
/// deletion sweep runs from the parent sync point to the end of that scope's
/// sync-point range, never into a neighbouring scope's rows.
#[tokio::test]
async fn partial_restore_stays_inside_its_scope() {
    let store = super::open_tmp();
    let a = api::Scope::new("d", "db", "a");
    let b = api::Scope::new("d", "db", "b");

    // Two commits in `a`, so the snapshot's second chunk carries a parent.
    add_key(&store, db_scope(&a), "k", b"1");
    add_key(&store, db_scope(&a), "k", b"2");
    // A scope sorting after `a`, which an unbounded sweep would run into.
    add_key(&store, db_scope(&b), "x", b"9");

    let full = {
        let tx = store
            .begin_local(&TransactionOptions::read())
            .expect("unable to start read tx");
        tx.take_snapshot(db_scope(&a)).expect("snapshot failed")
    };
    assert_eq!(full.len(), 2);
    assert!(
        full[1].meta.parent.is_some(),
        "the second commit's chunk names its parent",
    );

    let partial = vec![full[1].clone()];

    let mut tx = store
        .begin_local(&TransactionOptions::write())
        .expect("unable to start write tx");
    tx.restore_snapshot(db_scope(&a), partial)
        .expect("a parented restore must succeed without leaving its scope");
    tx.commit().expect("unable to commit");

    assert_eq!(
        get(&store, db_scope(&b), "x").as_deref(),
        Some(&b"9"[..]),
        "the neighbouring scope's data survives",
    );
    let tx = store
        .begin_local(&TransactionOptions::read())
        .expect("unable to start read tx");
    assert_eq!(
        tx.take_snapshot(db_scope(&b))
            .expect("snapshot failed")
            .len(),
        1,
        "the neighbouring scope's sync point survives",
    );
    assert_eq!(
        get(&store, db_scope(&a), "k").as_deref(),
        Some(&b"2"[..]),
        "the restored value is visible",
    );
}

/// A database from before the version index existed is refused outright —
/// pre-release, so no migration: the fix is to create a new database.
#[tokio::test]
async fn an_unindexed_database_is_refused() {
    let store = super::open_tmp();
    let scope = api::Scope::new("d", "db", "schema");

    // A fresh database is stamped at init and passes.
    store
        .check_version_index()
        .expect("a stamped database passes");

    add_key(&store, db_scope(&scope), "a", b"1");

    // Simulate a database from before the version index existed.
    {
        let mut tx = store
            .begin_local(&TransactionOptions::write())
            .expect("unable to start write tx");
        tx.strip_version_index_for_test()
            .expect("unable to strip index");
        tx.commit().expect("unable to commit");
    }

    let err = store
        .check_version_index()
        .expect_err("an unindexed database with data must be refused");
    assert!(
        err.to_string().contains("create a new database"),
        "the error tells the user what to do: {err}",
    );
}

/// A put over a delete of the same key in one tx clears the delete locally, so
/// the served changeset must carry only the insertion.
#[tokio::test]
async fn changeset_serving_omits_a_cleared_same_version_delete() {
    let store = super::open_tmp();
    let scope = api::Scope::new("d", "db", "schema");
    let dscope = db_scope(&scope);

    let mut tx = store
        .begin_local(&TransactionOptions::write())
        .expect("unable to start write tx");
    tx.key_delete(dscope.kv("a")).expect("unable to delete");
    tx.key_put(dscope.kv("a"), b"2").expect("unable to put");
    tx.commit().expect("unable to commit");

    let tx = store
        .begin_local(&TransactionOptions::read())
        .expect("unable to start read tx");
    let chunks = tx.take_snapshot(dscope).expect("snapshot failed");

    assert_eq!(chunks.len(), 1);
    let entries = &chunks[0].entries;
    assert_eq!(
        entries.len(),
        1,
        "the cleared delete must not be served: {entries:?}",
    );
    assert_eq!(entries[0].1.as_deref(), Some(&b"2"[..]));
}

#[tokio::test]
async fn stopping_replication_stops_the_replicator() {
    let subject = domain::Subject::Namespace("d".to_string());
    let (store, _transport, _replica) = spawn_replica(&subject);

    let scope = api::Scope::new("d", "db", "schema");
    add_key(&store, db_scope(&scope), "a", b"1");
    assert!(store.is_replicating(&scope));

    store.stop_replication(&subject);

    assert!(
        !store.is_replicating(&scope),
        "a stopped replicator no longer covers its subject",
    );
    assert_eq!(
        store.stray_scopes().unwrap(),
        vec![scope],
        "the stopped subject's local data is stray, ready to offload",
    );
}

#[tokio::test]
async fn replication_can_restart_after_a_stop() {
    let subject = domain::Subject::Namespace("d".to_string());
    let (store, transport, _replica) = spawn_replica(&subject);

    let scope = api::Scope::new("d", "db", "schema");
    add_key(&store, db_scope(&scope), "a", b"1");

    store.stop_replication(&subject);
    assert!(!store.is_replicating(&scope));

    let restarted = store.replicate(transport.clone(), subject.clone());
    assert!(
        restarted.is_some(),
        "a stopped subject can be replicated again",
    );
    assert!(store.is_replicating(&scope));
}

#[tokio::test]
async fn a_replicated_scope_is_not_offloadable() {
    let store = super::open_tmp();
    let transport = CaptureTransport::new();

    let subject = domain::Subject::Namespace("d".to_string());
    let _replica = store.replicate(transport.clone(), subject).unwrap();

    let scope = api::Scope::new("d", "db", "schema");
    assert!(
        store.offload(transport, scope).is_none(),
        "a scope covered by replication needs no offloader",
    );
}

#[tokio::test]
async fn stray_scopes_are_the_unreplicated_ones() {
    let store = super::open_tmp();
    let transport = CaptureTransport::new();

    let replicated = api::Scope::new("d", "db", "schema");
    let stray = api::Scope::new("e", "db", "schema");

    add_key(&store, db_scope(&replicated), "a", b"1");
    add_key(&store, db_scope(&stray), "b", b"2");

    let _replica = store
        .replicate(
            transport.clone(),
            domain::Subject::Namespace("d".to_string()),
        )
        .unwrap();

    assert_eq!(store.stray_scopes().unwrap(), vec![stray.clone()]);

    // An offloader claims the stray scope, so it is no longer reported.
    let _offloader = store.offload(transport, stray).unwrap();
    assert!(store.stray_scopes().unwrap().is_empty());
}

fn frontier_at(head: u64) -> ScopeAnnounce {
    let mut heads = ScopeFrontier::new();
    heads.insert(head, (0, [0u8; 16]));
    ScopeAnnounce::full(heads)
}

mod catchup_plan {
    use crate::replication::{CatchupPlan, plan_catchup};
    use db_commons::models::replication::{ScopeAnnounce, ScopeFrontier, head_fingerprint};

    const A: [u8; 16] = [1u8; 16];
    const B: [u8; 16] = [2u8; 16];

    fn frontier(entries: &[(u64, u64, [u8; 16])]) -> ScopeFrontier {
        entries
            .iter()
            .map(|&(ts, epoch, node)| (ts, (epoch, node)))
            .collect()
    }

    fn fold(entries: &[(u64, u64, [u8; 16])]) -> u64 {
        entries.iter().fold(0, |acc, &(ts, epoch, node)| {
            acc ^ head_fingerprint(ts, epoch, &node)
        })
    }

    #[test]
    fn fingerprint_is_order_independent_and_epoch_sensitive() {
        let forward = fold(&[(10, 10, A), (20, 20, A)]);
        let reverse = fold(&[(20, 20, A), (10, 10, A)]);
        assert_eq!(forward, reverse);

        let bumped = fold(&[(10, 10, A), (20, 90, A)]);
        assert_ne!(forward, bumped, "an epoch bump must change the fold");
        assert_ne!(forward, 0);
    }

    #[test]
    fn matching_fold_starts_the_cursor_at_the_baseline() {
        let ours = frontier(&[(10, 10, A), (20, 20, A), (35, 35, A)]);
        let theirs = ScopeAnnounce {
            baseline: Some(30),
            fingerprint: fold(&[(10, 10, A), (20, 20, A)]),
            heads: frontier(&[(35, 35, A), (40, 40, B)]),
        };

        assert_eq!(
            plan_catchup(Some(&ours), &theirs),
            CatchupPlan::Behind {
                since_ts: Some(35),
                epoch_floors: Default::default(),
            },
        );
    }

    #[test]
    fn mismatched_fold_diverges_instead_of_requesting() {
        let ours = frontier(&[(10, 10, A), (35, 35, A)]);
        let theirs = ScopeAnnounce {
            baseline: Some(30),
            fingerprint: fold(&[(10, 10, A), (20, 20, A)]),
            heads: frontier(&[(40, 40, B)]),
        };

        assert_eq!(plan_catchup(Some(&ours), &theirs), CatchupPlan::Diverged);
    }

    #[test]
    fn an_empty_store_diverges_from_a_nonempty_prefix() {
        let theirs = ScopeAnnounce {
            baseline: Some(30),
            fingerprint: fold(&[(10, 10, A)]),
            heads: ScopeFrontier::new(),
        };

        assert_eq!(plan_catchup(None, &theirs), CatchupPlan::Diverged);
    }

    #[test]
    fn a_missing_epoch_spike_below_the_cut_gets_a_zero_floor() {
        // Their head at ts 15 was re-stamped (epoch 90 > baseline), so it is
        // explicit and absent from both folds; we never saw ts 15 at all.
        let ours = frontier(&[(10, 10, A), (20, 20, A)]);
        let theirs = ScopeAnnounce {
            baseline: Some(30),
            fingerprint: fold(&[(10, 10, A), (20, 20, A)]),
            heads: frontier(&[(15, 90, B)]),
        };

        assert_eq!(
            plan_catchup(Some(&ours), &theirs),
            CatchupPlan::Behind {
                since_ts: Some(30),
                epoch_floors: [(15, 0)].into_iter().collect(),
            },
        );
    }

    #[test]
    fn a_stale_epoch_spike_below_the_cut_floors_at_our_epoch() {
        let ours = frontier(&[(10, 10, A), (15, 50, B), (20, 20, A)]);
        let theirs = ScopeAnnounce {
            baseline: Some(30),
            fingerprint: fold(&[(10, 10, A), (20, 20, A)]),
            heads: frontier(&[(15, 90, B)]),
        };

        assert_eq!(
            plan_catchup(Some(&ours), &theirs),
            CatchupPlan::Behind {
                since_ts: Some(30),
                epoch_floors: [(15, 50)].into_iter().collect(),
            },
        );
    }

    #[test]
    fn a_sufficient_epoch_spike_is_caught_up() {
        let ours = frontier(&[(10, 10, A), (15, 95, B), (20, 20, A)]);
        let theirs = ScopeAnnounce {
            baseline: Some(30),
            fingerprint: fold(&[(10, 10, A), (20, 20, A)]),
            heads: frontier(&[(15, 90, B)]),
        };

        assert_eq!(plan_catchup(Some(&ours), &theirs), CatchupPlan::CaughtUp);
    }

    #[test]
    fn a_full_announce_walks_the_prefix_from_scratch() {
        let ours = frontier(&[(10, 10, A), (30, 30, A)]);
        let theirs = ScopeAnnounce::full(frontier(&[(10, 10, A), (20, 20, A), (30, 30, A)]));

        assert_eq!(
            plan_catchup(Some(&ours), &theirs),
            CatchupPlan::Behind {
                since_ts: Some(10),
                epoch_floors: [(30, 30)].into_iter().collect(),
            },
        );
    }
}

#[tokio::test]
async fn a_floored_announce_elides_lagged_heads() {
    let subject = domain::Subject::Namespace("d".to_string());
    let (store, transport, mut replica) = spawn_replica(&subject);
    // Zero lag: every held head is old enough to fold under the baseline.
    replica.set_lag(Duration::ZERO);

    let scope = api::Scope::new("d", "db", "schema");
    add_key(&store, db_scope(&scope), "a", b"1");
    add_key(&store, db_scope(&scope), "b", b"2");

    replica.announce().await.expect("unable to announce");

    let mut messages = transport.drain_outgoing();
    assert_eq!(messages.len(), 1);
    let ReplicaMessage::Announce(announce) = messages.pop().unwrap() else {
        panic!("it should be an announce");
    };

    let sa = announce.known.get(&scope).expect("the scope is announced");
    assert!(sa.heads.is_empty(), "every head folds under a zero lag");
    assert!(sa.baseline.is_some());
    assert_ne!(sa.fingerprint, 0);
}

/// A peer that cannot verify the elided prefix probes for a full announce and
/// converges through it, after which the floored announces go quiet.
#[tokio::test]
async fn a_diverged_prefix_heals_through_a_probed_full_announce() {
    let subject = domain::Subject::Namespace("d".to_string());

    let (store1, transport1, mut replica1) = spawn_replica(&subject);
    let (store2, transport2, mut replica2) = spawn_replica(&subject);
    replica1.set_lag(Duration::ZERO);
    replica2.set_lag(Duration::ZERO);

    let scope = api::Scope::new("d", "db", "schema");
    add_key(&store1, db_scope(&scope), "a", b"1");

    // The floored announce elides everything, so the empty peer must probe.
    replica1.announce().await.expect("unable to announce");
    for msg in transport1.drain_outgoing() {
        replica2.clone().handle_message(transport1.id, msg).await;
    }
    let mut probes = transport2.drain_outgoing();
    assert_eq!(probes.len(), 1);
    let probe = probes.pop().unwrap();
    assert!(
        matches!(&probe, ReplicaMessage::Probe(p) if p.filter == vec![scope.clone()]),
        "an unverifiable prefix is probed, not blind-requested",
    );

    // The probe answers with a full announce, and the usual precise catch-up runs.
    replica1.clone().handle_message(transport2.id, probe).await;
    let mut full = transport1.drain_outgoing();
    assert_eq!(full.len(), 1);
    {
        let ReplicaMessage::Announce(announce) = &full[0] else {
            panic!("a probe answers with an announce");
        };
        let sa = announce.known.get(&scope).expect("the scope is announced");
        assert!(sa.baseline.is_none(), "a probe answer is never floored");
        assert!(!sa.heads.is_empty());
    }
    replica2
        .clone()
        .handle_message(transport1.id, full.pop().unwrap())
        .await;

    for msg in transport2.drain_outgoing() {
        replica1.clone().handle_message(transport2.id, msg).await;
    }
    for msg in transport1.drain_outgoing() {
        replica2.clone().handle_message(transport1.id, msg).await;
    }

    assert_eq!(
        get(&store2, db_scope(&scope), "a").as_deref(),
        Some(&b"1"[..]),
        "the peer converges through the probed full announce",
    );

    // Converged: the next floored announce verifies and stays quiet.
    replica1.announce().await.expect("unable to announce");
    for msg in transport1.drain_outgoing() {
        replica2.clone().handle_message(transport1.id, msg).await;
    }
    assert!(
        transport2.drain_outgoing().is_empty(),
        "a verified prefix needs neither probe nor request",
    );
}

/// With elided announces on the replica side, an offloader's old holdings are
/// invisible in the periodic announce; retirement runs off the full announce
/// its own probe solicits.
#[tokio::test]
async fn an_offloader_retires_through_its_probe_when_the_replica_elides() {
    let scope = api::Scope::new("d", "db", "schema");
    let (store1, transport1, offloader) = spawn_offloader(&scope);

    let subject = domain::Subject::Namespace("d".to_string());
    let (store2, transport2, mut replica) = spawn_replica(&subject);
    replica.set_lag(Duration::ZERO);

    add_key(&store1, db_scope(&scope), "a", b"1");

    // Drain the data across.
    settle(&offloader, &transport1, &replica, &transport2).await;
    assert_eq!(
        get(&store2, db_scope(&scope), "a").as_deref(),
        Some(&b"1"[..])
    );

    // The replica's floored announce elides the pulled data, so it cannot
    // vouch for the offloader's holdings.
    replica.announce().await.expect("unable to announce");
    for msg in transport2.drain_outgoing() {
        offloader.clone().handle_message(transport2.id, msg).await;
    }
    assert!(
        store1.is_offloading(&scope),
        "an elided prefix must not retire the offloader",
    );

    // The offloader's own announce probes for a full announce, which does.
    offloader.announce().await.expect("unable to announce");
    for msg in transport1.drain_outgoing() {
        replica.clone().handle_message(transport1.id, msg).await;
    }
    for msg in transport2.drain_outgoing() {
        offloader.clone().handle_message(transport2.id, msg).await;
    }
    assert!(
        !store1.is_offloading(&scope),
        "the offloader retires through the full announce its probe solicits",
    );
}

#[tokio::test]
async fn a_fresh_drains_solicit_fills_its_peer_view_within_a_round_trip() {
    // A fallback-minted sink starts blind: without soliciting, its peer view
    // only fills on a replica's next periodic announce, and it absorbs stray
    // writes in the meantime. Soliciting makes a live replica answer now, so
    // the quiesce check can refuse the very next routed write.
    let scope = api::Scope::new("d", "db", "schema");
    let (store1, transport1, drain) = spawn_offloader(&scope);

    let subject = domain::Subject::Namespace("d".to_string());
    let (store2, transport2, replica) = spawn_replica(&subject);
    add_key(&store2, db_scope(&scope), "a", b"1");

    drain.solicit(&scope).await;
    // Leg one delivers the probe; leg two delivers the answering announce.
    exchange(&drain, &transport1, &replica, &transport2).await;
    exchange(&drain, &transport1, &replica, &transport2).await;

    let view = store1.peer_view(&scope, std::time::Instant::now());
    assert!(
        view.iter().any(|peer| {
            peer.id == transport2.id.to_le_bytes()
                && matches!(peer.state, db_commons::models::locate::HolderState::Replica)
        }),
        "the solicited full announce must vouch the replica in the drain's peer view",
    );
}

#[tokio::test]
async fn peer_view_reports_the_baseline_of_a_fully_elided_announce() {
    let store = super::open_tmp();
    let scope = api::Scope::new("d", "db", "schema");

    let mut known = VecMap::new();
    known.insert(
        scope.clone(),
        ScopeAnnounce {
            baseline: Some(7),
            fingerprint: 42,
            heads: ScopeFrontier::new(),
        },
    );
    store.record_peer_frontier([1u8; 16], known, true);

    let view = store.peer_view(&scope, std::time::Instant::now());

    assert_eq!(view.len(), 1);
    assert_eq!(
        view[0].head, 7,
        "the baseline is capped at the peer's newest head, so it is exact",
    );
}

#[tokio::test]
async fn peer_view_lists_live_peers_holding_the_scope() {
    let store = super::open_tmp();
    let scope = api::Scope::new("d", "db", "schema");

    let mut known = VecMap::new();
    known.insert(scope.clone(), frontier_at(7));
    store.record_peer_frontier([1u8; 16], known, true);

    let view = store.peer_view(&scope, std::time::Instant::now());

    assert_eq!(view.len(), 1);
    assert_eq!(view[0].id, [1u8; 16]);
    assert_eq!(view[0].head, 7);
    assert!(
        matches!(
            view[0].state,
            db_commons::models::locate::HolderState::Replica
        ),
        "a full-replica announce vouches the peer as a replica",
    );
}

#[tokio::test]
async fn peer_view_reports_a_drainer_as_draining() {
    let store = super::open_tmp();
    let scope = api::Scope::new("d", "db", "schema");

    let mut known = VecMap::new();
    known.insert(scope.clone(), frontier_at(7));
    store.record_peer_frontier([1u8; 16], known, false);

    let view = store.peer_view(&scope, std::time::Instant::now());

    assert_eq!(view.len(), 1);
    assert!(
        matches!(
            view[0].state,
            db_commons::models::locate::HolderState::Draining
        ),
        "an offload announce vouches the peer only as a drainer",
    );
}

#[tokio::test]
async fn peer_view_merges_announces_across_subjects() {
    // One node can replicate subject A while draining scope B; each announce
    // travels on its own channel. Recording one must not clobber the other,
    // and each scope keeps the state of the announce that mentioned it.
    let store = super::open_tmp();
    let replicated = api::Scope::new("d", "db", "schema");
    let drained = api::Scope::new("e", "db", "schema");

    let mut known = VecMap::new();
    known.insert(replicated.clone(), frontier_at(7));
    store.record_peer_frontier([1u8; 16], known, true);

    let mut known = VecMap::new();
    known.insert(drained.clone(), frontier_at(3));
    store.record_peer_frontier([1u8; 16], known, false);

    let now = std::time::Instant::now();

    let view = store.peer_view(&replicated, now);
    assert_eq!(
        view.len(),
        1,
        "the later announce must not clobber this one"
    );
    assert_eq!(view[0].head, 7);
    assert!(matches!(
        view[0].state,
        db_commons::models::locate::HolderState::Replica
    ));

    let view = store.peer_view(&drained, now);
    assert_eq!(view.len(), 1);
    assert_eq!(view[0].head, 3);
    assert!(matches!(
        view[0].state,
        db_commons::models::locate::HolderState::Draining
    ));
}

#[tokio::test]
async fn peer_view_excludes_peers_last_seen_beyond_ttl() {
    let store = super::open_tmp();
    let scope = api::Scope::new("d", "db", "schema");

    let mut known = VecMap::new();
    known.insert(scope.clone(), frontier_at(7));
    store.record_peer_frontier([1u8; 16], known, true);

    // Long enough after the peer was recorded that it is presumed gone.
    let now = std::time::Instant::now() + std::time::Duration::from_mins(1);
    let view = store.peer_view(&scope, now);

    assert!(
        view.is_empty(),
        "a peer beyond the TTL must not be vouched for"
    );
}

#[tokio::test]
async fn peer_view_excludes_peers_not_holding_the_scope() {
    let store = super::open_tmp();
    let held = api::Scope::new("d", "db", "schema");
    let other = api::Scope::new("e", "db", "schema");

    let mut known = VecMap::new();
    known.insert(other, frontier_at(7));
    store.record_peer_frontier([1u8; 16], known, true);

    let view = store.peer_view(&held, std::time::Instant::now());

    assert!(
        view.is_empty(),
        "a peer that does not hold the scope must not be listed"
    );
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "one linear protocol walkthrough")]
async fn replica_roundtrips() {
    let subject = domain::Subject::Namespace("d".to_string());

    let (store1, transport1, replica1) = spawn_replica(&subject);
    let (store2, transport2, replica2) = spawn_replica(&subject);

    let scope_a = api::Scope::new("d", "db_a", "schema_a");
    let scope_b = api::Scope::new("d", "db_b", "schema_b");

    add_key(&store1, db_scope(&scope_a), "a", b"1");
    add_key(&store2, db_scope(&scope_b), "b", b"2");

    replica1.announce().await.expect("unable to read db");
    replica2.announce().await.expect("unable to read db");

    // Check the first transport.
    let (msg1, baseline_a) = {
        let mut messages = transport1.drain_outgoing();
        assert_eq!(messages.len(), 1);
        let msg = messages.pop().expect("There should be exactly one message");
        let ReplicaMessage::Announce(announce) = &msg else {
            panic!("it should be an announce");
        };
        assert_eq!(announce.known.len(), 1);
        let sa = announce.known.get(&scope_a).expect("scope_a is announced");
        assert!(
            sa.baseline.is_some(),
            "a full replica's periodic announce is floored",
        );
        assert_eq!(
            sa.heads.len(),
            1,
            "a fresh head is younger than the lag, so it stays explicit",
        );
        let baseline = sa.baseline;

        (msg, baseline)
    };

    // Check the second transport.
    let (msg2, baseline_b) = {
        let mut messages = transport2.drain_outgoing();
        assert_eq!(messages.len(), 1);
        let msg = messages.pop().expect("There should be exactly one message");
        let ReplicaMessage::Announce(announce) = &msg else {
            panic!("it should be an announce");
        };
        assert_eq!(announce.known.len(), 1);
        let sa = announce.known.get(&scope_b).expect("scope_b is announced");
        let baseline = sa.baseline;

        (msg, baseline)
    };

    // Forward the second to the first, and vice versa.
    replica1.clone().handle_message(transport2.id, msg2).await;
    replica2.clone().handle_message(transport1.id, msg1).await;

    let msg1 = {
        let mut messages = transport1.drain_outgoing();
        assert_eq!(messages.len(), 1);
        let msg = messages.pop().expect("There should be exactly one message");
        let ReplicaMessage::ChangeSetReq(req) = &msg else {
            panic!("it should be a changeset req");
        };

        assert_eq!(req.scope, scope_b);
        assert_eq!(
            req.since_ts, baseline_b,
            "the verified baseline is the catch-up cursor",
        );
        assert!(req.epoch_floors.is_empty());

        msg
    };

    let msg2 = {
        let mut messages = transport2.drain_outgoing();
        assert_eq!(messages.len(), 1);
        let msg = messages.pop().expect("There should be exactly one message");
        let ReplicaMessage::ChangeSetReq(req) = &msg else {
            panic!("it should be a changeset req");
        };

        assert_eq!(req.scope, scope_a);
        assert_eq!(
            req.since_ts, baseline_a,
            "the verified baseline is the catch-up cursor",
        );
        assert!(req.epoch_floors.is_empty());

        msg
    };

    // Forward the second to the first, and vice versa.
    replica1.clone().handle_message(transport2.id, msg2).await;
    replica2.clone().handle_message(transport1.id, msg1).await;

    let msg1 = {
        let mut messages = transport1.drain_outgoing();
        assert_eq!(messages.len(), 1);
        let msg = messages.pop().expect("There should be exactly one message");
        let ReplicaMessage::ChangeSet(cs) = &msg else {
            panic!("it should be a changeset");
        };

        assert_eq!(cs.scope, scope_a);

        msg
    };

    let msg2 = {
        let mut messages = transport2.drain_outgoing();
        assert_eq!(messages.len(), 1);
        let msg = messages.pop().expect("There should be exactly one message");
        let ReplicaMessage::ChangeSet(cs) = &msg else {
            panic!("it should be a changeset");
        };

        assert_eq!(cs.scope, scope_b);

        msg
    };

    // Forward the second to the first, and vice versa.
    replica1.clone().handle_message(transport2.id, msg2).await;
    replica2.clone().handle_message(transport1.id, msg1).await;

    {
        assert!(transport1.drain_outgoing().is_empty());
        assert!(transport2.drain_outgoing().is_empty());
    }
}

#[tokio::test]
#[rustfmt::skip]
async fn replica_tracks_active() {
    let subject = domain::Subject::Namespace("d".to_string());

    let (store1, transport1, replica1) = spawn_replica(&subject);
    let (store2, transport2, replica2) = spawn_replica(&subject);

    let scope_a = api::Scope::new("d", "db_a", "schema_a");
    let scope_b = api::Scope::new("d", "db_b", "schema_b");

    add_key(
        &store1,
        db_scope(&scope_a),
        "a",
        b"1",
    );
    add_key(
        &store2,
        db_scope(&scope_b),
        "b",
        b"2",
    );

    // We don't store replica info for our own local stuff, as we can't know if something was tracked before it was requested.
    // This might change, ie, we might only offer up scopes _once_ we know that we've definitely got the latest.
    // But as it stands, unless something requests the scope, we assume we're the boss.
    // Our replication protocol can handle concurrent operations.
    assert!(store1.replica_status(&scope_a).unwrap().is_none());
    assert!(store1.replica_status(&scope_b).unwrap().is_none());
    assert!(store2.replica_status(&scope_a).unwrap().is_none());
    assert!(store2.replica_status(&scope_b).unwrap().is_none());

    replica1.announce().await.expect("unable to read db");
    replica2.announce().await.expect("unable to read db");

    // Exchange announce
    exchange(&replica1, &transport1, &replica2, &transport2).await;

    // Again, just to reiterate, if no one provides evidence that we're behind then we don't track the state.
    // So the only thing that's changed here is that these two replicas learnt about the _other_ scope, ie 1 learnt about b, 2 learnt about a.
    // And they're now tracking it, trying to work out when they can switch to an active state.
    assert!(store1.replica_status(&scope_a).unwrap().is_none());
    assert_eq!(store1.replica_status(&scope_b).unwrap(), Some(ReplicationStatus::Requested));
    assert_eq!(store2.replica_status(&scope_a).unwrap(), Some(ReplicationStatus::Requested));
    assert!(store2.replica_status(&scope_b).unwrap().is_none());

    // There's now reqs in the queue
    assert_eq!(transport1.len(), 1);
    assert_eq!(transport2.len(), 1);

    // Exchange req
    exchange(&replica1, &transport1, &replica2, &transport2).await;

    assert!(store1.replica_status(&scope_a).unwrap().is_none());
    assert_eq!(store1.replica_status(&scope_b).unwrap(), Some(ReplicationStatus::Requested));
    assert_eq!(store2.replica_status(&scope_a).unwrap(), Some(ReplicationStatus::Requested));
    assert!(store2.replica_status(&scope_b).unwrap().is_none());

    // There's now changesets waiting
    // We're not letting the first replica send it's changeset to the second replica.
    // This should cause the first replica to transistion to active,
    // while the second replica stays in a requested state, as it's not caught up yet.
    let msgs = transport1.drain_outgoing();
    assert_eq!(msgs.len(), 1);
    assert_eq!(transport2.len(), 1);

    // Exchange changesets (this performs the transistion)
    exchange(&replica1, &transport1, &replica2, &transport2).await;

    // And here is where we check that.
    assert!(store1.replica_status(&scope_a).unwrap().is_none());
    assert_eq!(store1.replica_status(&scope_b).unwrap(), Some(ReplicationStatus::Active));
    assert_eq!(store2.replica_status(&scope_a).unwrap(), Some(ReplicationStatus::Requested));
    assert!(store2.replica_status(&scope_b).unwrap().is_none());
}
