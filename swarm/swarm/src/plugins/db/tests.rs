use std::time::Duration;

use super::Plugin;
use crate::plugins::{MyrmicCtx, MyrmicPlugin};
use cell_protocol::node_tags::LiveTags;
use cell_protocol::replication::{
    CUSTODY_TABLE, CustodyRow, REPLICATION_TABLE, ReplicaEntry, ReplicaSelector, replication_scope,
    runtime_tag,
};
use db_client::application::Application;
use db_client::v1::Client;
use db_commons::models::events::TableEvent;
use db_commons::models::{self, Scope, Subject};

const TABLE: &str = "letters";

async fn open_session() -> zenoh::Session {
    let mut config = zenoh::Config::default();
    config
        .insert_json5("timestamping/enabled", "{ peer: true }")
        .unwrap();
    config
        .insert_json5("scouting/multicast/enabled", "false")
        .unwrap();

    zenoh::open(config).await.expect("unable to open session")
}

/// A plugin context for one node, tagged the way the host tags it at boot —
/// tests pin replicas by runtime tag, which lives in that set.
fn ctx(session: &zenoh::Session, drop_rx: swarm_api::DropNotifier) -> MyrmicCtx {
    let tags = LiveTags::new(crate::node_tags::effective_at_boot(session, &[]));

    MyrmicCtx::new(
        session.clone(),
        tokio::runtime::Handle::current(),
        Default::default(),
        tags,
        drop_rx,
        swarm_api::Ready::default(),
    )
}

async fn start_node() -> (zenoh::Session, swarm_api::DropSender) {
    let session = open_session().await;

    let (drop_tx, drop_rx) = flume::bounded(1);

    Plugin::main(ctx(&session, drop_rx), Default::default())
        .await
        .expect("unable to start db plugin");

    (session, drop_tx)
}

/// Like [`start_node`], but with a short offload-escalation timeout so a test
/// can observe an uncovered offloader escalating to a replica without waiting
/// out the production default.
async fn start_node_with_escalation(
    escalation: Duration,
) -> (zenoh::Session, swarm_api::DropSender) {
    let session = open_session().await;

    let (drop_tx, drop_rx) = flume::bounded(1);

    let config = super::config::Config {
        store: super::config::StoreConfig {
            offload_escalation_timeout: Some(escalation),
            ..Default::default()
        },
        ..Default::default()
    };

    Plugin::main(ctx(&session, drop_rx), config)
        .await
        .expect("unable to start db plugin");

    (session, drop_tx)
}

async fn read_row(client: &Client, table: &str, key: &str) -> Option<Vec<u8>> {
    let tx = client
        .send(models::tx_begin::Request::default())
        .await
        .expect("send failed")
        .expect("tx begin failed");

    let row = client
        .send(models::tb_get::Request {
            id: tx.id,
            op: models::tb_get::Op {
                scope: replication_scope(),
                table: table.into(),
                eid: key.as_bytes().to_vec(),
            },
        })
        .await
        .expect("send failed")
        .expect("get failed");

    client
        .send(models::tx_rollback::Request { id: tx.id })
        .await
        .expect("send failed")
        .expect("rollback failed");

    row.value
}

async fn read_replica_entry(client: &Client, key: &str) -> Option<ReplicaEntry> {
    read_row(client, REPLICATION_TABLE, key)
        .await
        .and_then(|value| postcard::from_bytes::<ReplicaEntry>(&value).ok())
}

async fn read_custody_row(client: &Client, key: &str) -> Option<CustodyRow> {
    read_row(client, CUSTODY_TABLE, key)
        .await
        .and_then(|value| postcard::from_bytes::<CustodyRow>(&value).ok())
}

/// This node's custody-row key for `scope`.
fn custody_key(session: &zenoh::Session, scope: &Scope) -> String {
    let id: uhlc::ID = session.zid().into();
    CustodyRow::new(scope.clone(), id.to_le_bytes()).key()
}

fn scope() -> Scope {
    Scope::new("testing", "events", "public")
}

async fn insert_one(client: &Client, tx_id: models::TxId, scope: &Scope) {
    client
        .send(models::tb_insert::Request {
            id: tx_id,
            op: models::tb_insert::Op {
                scope: scope.clone(),
                table: TABLE.into(),
                eid: None,
                value: b"a".to_vec(),
            },
        })
        .await
        .expect("send failed")
        .expect("insert failed");
}

#[tokio::test(flavor = "multi_thread")]
async fn commit_publishes_insert_event() {
    let (session, _drop_tx) = start_node().await;
    let client = Client::new(&session);

    let (events_tx, events_rx) = flume::unbounded();
    let _sub = client
        .subscribe(Subject::Scope(scope()), TABLE, move |notification| {
            let _ = events_tx.send(notification);
        })
        .await
        .expect("unable to subscribe");

    let tx = client
        .send(models::tx_begin::Request::default())
        .await
        .expect("send failed")
        .expect("tx begin failed");

    insert_one(&client, tx.id, &scope()).await;
    insert_one(&client, tx.id, &scope()).await;

    client
        .send(models::tx_commit::Request { id: tx.id })
        .await
        .expect("send failed")
        .expect("commit failed");

    let notification = tokio::time::timeout(Duration::from_secs(10), events_rx.recv_async())
        .await
        .expect("no event within 10s")
        .expect("event channel closed");

    assert_eq!(notification.scope, scope());
    assert_eq!(notification.table, TABLE);

    assert!(matches!(notification.event, TableEvent::Inserted(_)));

    // Both inserts landed in one commit, so there is exactly one poke.
    let extra = tokio::time::timeout(Duration::from_millis(500), events_rx.recv_async()).await;
    assert!(
        extra.is_err(),
        "expected a single event per table per commit"
    );
}

/// Configures a replication set pinning `subject` to the node (via its runtime
/// tag), so a locate queryable covering it is declared (a routed `tx_begin`
/// finds a holder via locate over replicating nodes). The watcher applies the
/// entry asynchronously — callers needing a holder poll via [`locate_eventually`].
async fn replicate(client: &Client, session: &zenoh::Session, subject: Subject) {
    let selector = ReplicaSelector::Subject(subject);
    let label = selector.to_string();
    let entry = ReplicaEntry::new(selector, vec![runtime_tag(session.zid().into())], &label);

    let tx = client
        .send(models::tx_begin::Request::default())
        .await
        .expect("send failed")
        .expect("tx begin failed");

    client
        .send(models::tb_insert::Request {
            id: tx.id,
            op: models::tb_insert::Op {
                scope: replication_scope(),
                table: REPLICATION_TABLE.into(),
                eid: Some(entry.key().into_bytes()),
                value: postcard::to_allocvec(&entry).expect("entry should serialise"),
            },
        })
        .await
        .expect("send failed")
        .expect("insert failed");

    client
        .send(models::tx_commit::Request { id: tx.id })
        .await
        .expect("send failed")
        .expect("commit failed");
}

/// Polls locate until a holder of `scope` at `min_version` answers, so a test
/// only proceeds once the replication watcher has applied the configuration.
async fn locate_eventually(
    replica: &db_client::replica_v1::Client,
    scope: &Scope,
    min_version: Option<models::Version>,
) -> Vec<db_client::replica_v1::Located> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);

    loop {
        let holders = replica
            .locate(scope, min_version)
            .await
            .expect("locate failed");
        if !holders.is_empty() {
            return holders;
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "no replicating holder answered within 10s"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn commit_and_await_version(
    client: &Client,
    events_rx: &flume::Receiver<models::events::Notification>,
) -> models::Version {
    let tx = client
        .send(models::tx_begin::Request::default())
        .await
        .expect("send failed")
        .expect("tx begin failed");

    insert_one(client, tx.id, &scope()).await;

    client
        .send(models::tx_commit::Request { id: tx.id })
        .await
        .expect("send failed")
        .expect("commit failed");

    let notification = tokio::time::timeout(Duration::from_secs(10), events_rx.recv_async())
        .await
        .expect("no event within 10s")
        .expect("event channel closed");

    notification.version()
}

#[tokio::test(flavor = "multi_thread")]
async fn locate_finds_a_replicating_holder_at_version() {
    let (session, _drop_tx) = start_node().await;
    let client = Client::new(&session);

    replicate(&client, &session, Subject::Scope(scope())).await;

    let (events_tx, events_rx) = flume::unbounded();
    let _sub = client
        .subscribe(Subject::Scope(scope()), TABLE, move |notification| {
            let _ = events_tx.send(notification);
        })
        .await
        .expect("unable to subscribe");

    let version = commit_and_await_version(&client, &events_rx).await;

    let replica = db_client::replica_v1::Client::new(&session, Subject::Scope(scope()))
        .expect("unable to create replica client");

    // At the head version, the sole replicating node answers.
    let holders = locate_eventually(&replica, &scope(), Some(version)).await;
    assert_eq!(holders.len(), 1, "the replicating holder should answer");
    assert!(holders[0].head >= version);

    // Far past the head, nobody is caught up, so nobody answers.
    let unreached = replica
        .locate(&scope(), Some(version + (1 << 40)))
        .await
        .expect("locate failed");
    assert!(
        unreached.is_empty(),
        "no node should claim a version it has not reached"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn locate_hears_a_replica_holding_no_data() {
    let (session, _drop_tx) = start_node().await;
    let client = Client::new(&session);

    replicate(&client, &session, Subject::Scope(scope())).await;

    // Nothing was ever committed to the scope. The configured replica still
    // answers (at head 0), so a first write routes to it instead of falling
    // back and promoting some unrelated node.
    let replica = db_client::replica_v1::Client::new(&session, Subject::Scope(scope()))
        .expect("unable to create replica client");

    let holders = locate_eventually(&replica, &scope(), None).await;
    assert_eq!(holders.len(), 1, "the empty replica should answer");
    assert_eq!(holders[0].head, 0, "an empty replica has no head yet");
}

#[tokio::test(flavor = "multi_thread")]
async fn tx_begin_resumes_from_an_event_version() {
    let (session, _drop_tx) = start_node().await;
    let client = Client::new(&session);

    replicate(
        &client,
        &session,
        Subject::Namespace(String::from("testing")),
    )
    .await;

    let (events_tx, events_rx) = flume::unbounded();
    let _sub = client
        .subscribe(Subject::Scope(scope()), TABLE, move |notification| {
            let _ = events_tx.send(notification);
        })
        .await
        .expect("unable to subscribe");

    let version = commit_and_await_version(&client, &events_rx).await;

    // Wait until the node answers locate for the scope at the version, so the
    // routed_at below deterministically finds it.
    let replica = db_client::replica_v1::Client::new(&session, Subject::Scope(scope()))
        .expect("unable to create replica client");
    locate_eventually(&replica, &scope(), Some(version)).await;

    // A replicating node holds the scope at the version, so the resumed tx begins.
    let resumed = client
        .send(models::tx_begin::Request::routed_at(scope(), version))
        .await
        .expect("send failed")
        .expect("resumed tx begin failed");

    client
        .send(models::tx_rollback::Request { id: resumed.id })
        .await
        .expect("send failed")
        .expect("rollback failed");
}

#[tokio::test(flavor = "multi_thread")]
async fn tx_begin_rejects_a_version_no_node_has_reached() {
    let (session, _drop_tx) = start_node().await;
    let client = Client::new(&session);

    replicate(
        &client,
        &session,
        Subject::Namespace(String::from("testing")),
    )
    .await;

    let (events_tx, events_rx) = flume::unbounded();
    let _sub = client
        .subscribe(Subject::Scope(scope()), TABLE, move |notification| {
            let _ = events_tx.send(notification);
        })
        .await
        .expect("unable to subscribe");

    let version = commit_and_await_version(&client, &events_rx).await;

    // A version far past what the (only) node has committed cannot be served.
    let result = client
        .send(models::tx_begin::Request::routed_at(
            scope(),
            version + (1 << 40),
        ))
        .await
        .expect("send failed");

    assert!(
        result.is_err(),
        "tx_begin must reject a version no node has reached"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_routed_fallback_makes_the_scope_locatable() {
    let (session, _drop_tx) = start_node().await;
    let client = Client::new(&session);

    // Nothing replicates the scope, so the routed begin falls back to this node.
    let tx = client
        .send(models::tx_begin::Request::routed(scope()))
        .await
        .expect("send failed")
        .expect("tx begin failed");

    client
        .send(models::tx_rollback::Request { id: tx.id })
        .await
        .expect("send failed")
        .expect("rollback failed");

    // The fallback holds the scope as a findable provisional offloader, so it
    // answers locate — no durable promotion on a single miss.
    let replica = db_client::replica_v1::Client::new(&session, Subject::Scope(scope()))
        .expect("unable to create replica client");
    let holders = locate_eventually(&replica, &scope(), None).await;
    assert!(
        matches!(holders[0].state, models::locate::HolderState::Draining),
        "a fallback sink answers locate as draining, so writes prefer real replicas",
    );
}

/// Escalates an uncovered fallback: a routed write begins (and rolls back) on
/// a node nothing replicates the scope on, then the provisional offloader
/// promotes itself once the (shortened) escalation window lapses.
async fn escalate(session: &zenoh::Session, client: &Client) {
    let tx = client
        .send(models::tx_begin::Request::routed(scope()))
        .await
        .expect("send failed")
        .expect("tx begin failed");

    client
        .send(models::tx_rollback::Request { id: tx.id })
        .await
        .expect("send failed")
        .expect("rollback failed");

    let key = custody_key(session, &scope());

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if read_custody_row(client, &key).await.is_some() {
            break;
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "an uncovered offloader must escalate to a custody-backed replica",
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_uncovered_fallback_escalates_to_a_custody_row() {
    // A short escalation window so the uncovered offloader promotes itself
    // within the test rather than after the production default.
    let (session, _drop_tx) = start_node_with_escalation(Duration::from_millis(300)).await;
    let client = Client::new(&session);

    // Nothing replicates the scope, so the routed begin falls back here, is
    // held as a provisional offloader, and escalates into a custody row.
    escalate(&session, &client).await;

    // Intent stays human-only: promotion must not touch the configured sets.
    let entry_key = ReplicaSelector::Subject(Subject::Scope(scope())).to_string();
    assert!(
        read_replica_entry(&client, &entry_key).await.is_none(),
        "promotion must never write into the configured replication sets",
    );

    // The promoted node is a full replica in every functional sense.
    let replica = db_client::replica_v1::Client::new(&session, Subject::Scope(scope()))
        .expect("unable to create replica client");
    let holders = locate_eventually(&replica, &scope(), None).await;
    assert!(
        matches!(holders[0].state, models::locate::HolderState::Replica),
        "a promoted custodian answers locate as a replica",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pinned_custodian_deletes_its_own_row() {
    let (session, _drop_tx) = start_node_with_escalation(Duration::from_millis(300)).await;
    let client = Client::new(&session);

    escalate(&session, &client).await;

    // A human pins the scope to this node: the provisional sees itself
    // configured and converts, deleting its own custody row. Replication
    // continues under the configured entry.
    replicate(&client, &session, Subject::Scope(scope())).await;

    let key = custody_key(&session, &scope());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if read_custody_row(&client, &key).await.is_none() {
            break;
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "a pinned custodian must delete its own custody row",
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let replica = db_client::replica_v1::Client::new(&session, Subject::Scope(scope()))
        .expect("unable to create replica client");
    let holders = locate_eventually(&replica, &scope(), None).await;
    assert!(
        matches!(holders[0].state, models::locate::HolderState::Replica),
        "the converted node keeps replicating under the configured entry",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_routed_read_fallback_leaves_no_trace() {
    // Short escalation window: were a read fallback wrongly held like a write
    // sink, it would escalate within the test and the assertions would see it.
    let (session, _drop_tx) = start_node_with_escalation(Duration::from_millis(300)).await;
    let client = Client::new(&session);

    // Nothing replicates the scope; the routed read falls back to this node.
    client
        .read_tx_in(scope(), async |_, _| Ok(()))
        .await
        .expect("routed read failed");

    // Past the escalation window: a read that located nobody must not have
    // made this node hold the scope, let alone promote itself for it.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let replica = db_client::replica_v1::Client::new(&session, Subject::Scope(scope()))
        .expect("unable to create replica client");
    let holders = replica.locate(&scope(), None).await.expect("locate failed");
    assert!(
        holders.is_empty(),
        "a read fallback must not make the scope locatable",
    );

    let key = custody_key(&session, &scope());
    assert!(
        read_custody_row(&client, &key).await.is_none(),
        "a read fallback must not escalate into custody",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_commit_to_an_unreplicated_scope_starts_offloading() {
    let (session, _drop_tx) = start_node().await;
    let client = Client::new(&session);

    // Watch the scope's replica channel raw: the replica client filters out
    // its own session's messages, and the node under test shares this one.
    let s = scope();
    let ke =
        db_commons::topics::replica::format_replica(&s.namespace, &s.database, &s.schema, "*", "*");

    let (announce_tx, announce_rx) = flume::unbounded();
    let _sub = session
        .declare_subscriber(ke)
        .callback(move |sample| {
            let payload = sample.payload().to_bytes();
            if let Ok(models::ReplicaMessage::Announce(announce)) = postcard::from_bytes(&payload) {
                let _ = announce_tx.send(announce);
            }
        })
        .await
        .expect("unable to subscribe to the replica channel");

    // An unconstrained commit: nothing was located or promoted, so the data
    // lands on a node that does not replicate its scope.
    let tx = client
        .send(models::tx_begin::Request::default())
        .await
        .expect("send failed")
        .expect("tx begin failed");

    insert_one(&client, tx.id, &scope()).await;

    client
        .send(models::tx_commit::Request { id: tx.id })
        .await
        .expect("send failed")
        .expect("commit failed");

    // The node must offer the stray data up so a replica can pull it off.
    loop {
        let announce = tokio::time::timeout(Duration::from_secs(10), announce_rx.recv_async())
            .await
            .expect("no offload announce within 10s")
            .expect("announce channel closed");

        if announce.known.contains_key(&scope()) {
            break;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_replication_stops_locate_and_starts_offloading() {
    let (session, _drop_tx) = start_node().await;
    let client = Client::new(&session);

    replicate(&client, &session, Subject::Scope(scope())).await;

    let (events_tx, events_rx) = flume::unbounded();
    let _sub = client
        .subscribe(Subject::Scope(scope()), TABLE, move |notification| {
            let _ = events_tx.send(notification);
        })
        .await
        .expect("unable to subscribe");

    commit_and_await_version(&client, &events_rx).await;

    let replica = db_client::replica_v1::Client::new(&session, Subject::Scope(scope()))
        .expect("unable to create replica client");
    locate_eventually(&replica, &scope(), None).await;

    // Watch the scope's replica channel raw, as in the offload test above.
    let s = scope();
    let ke =
        db_commons::topics::replica::format_replica(&s.namespace, &s.database, &s.schema, "*", "*");

    let (announce_tx, announce_rx) = flume::unbounded();
    let _raw = session
        .declare_subscriber(ke)
        .callback(move |sample| {
            let payload = sample.payload().to_bytes();
            if let Ok(models::ReplicaMessage::Announce(announce)) = postcard::from_bytes(&payload) {
                let _ = announce_tx.send(announce);
            }
        })
        .await
        .expect("unable to subscribe to the replica channel");

    // Drop this node from the entry by re-tagging it to nobody. An insert
    // wakes the watcher via the table event; a delete would wait out the poll.
    let selector = ReplicaSelector::Subject(Subject::Scope(scope()));
    let label = selector.to_string();
    let entry = ReplicaEntry::new(selector, vec![String::from("tag:nobody")], &label);

    let tx = client
        .send(models::tx_begin::Request::default())
        .await
        .expect("send failed")
        .expect("tx begin failed");

    client
        .send(models::tb_insert::Request {
            id: tx.id,
            op: models::tb_insert::Op {
                scope: replication_scope(),
                table: REPLICATION_TABLE.into(),
                eid: Some(entry.key().into_bytes()),
                value: postcard::to_allocvec(&entry).expect("entry should serialise"),
            },
        })
        .await
        .expect("send failed")
        .expect("insert failed");

    client
        .send(models::tx_commit::Request { id: tx.id })
        .await
        .expect("send failed")
        .expect("commit failed");

    // The replicator winds down, so locate stops offering this node up.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let holders = replica.locate(&scope(), None).await.expect("locate failed");
        if holders.is_empty() {
            break;
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "a dropped replica must stop answering locate"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // The data it still holds is offered up for the remaining replicas to
    // pull. A stopped replicator can have at most one announce in flight, so
    // a second one proves an offloader is running.
    announce_rx.drain();
    let mut announces = 0;
    while announces < 2 {
        let announce = tokio::time::timeout(Duration::from_secs(10), announce_rx.recv_async())
            .await
            .expect("no offload announce within 10s")
            .expect("announce channel closed");

        if announce.known.contains_key(&scope()) {
            announces += 1;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rollback_publishes_nothing() {
    let (session, _drop_tx) = start_node().await;
    let client = Client::new(&session);

    let (events_tx, events_rx) = flume::unbounded();
    let _sub = client
        .subscribe(Subject::Scope(scope()), TABLE, move |notification| {
            let _ = events_tx.send(notification);
        })
        .await
        .expect("unable to subscribe");

    let tx = client
        .send(models::tx_begin::Request::default())
        .await
        .expect("send failed")
        .expect("tx begin failed");

    insert_one(&client, tx.id, &scope()).await;

    client
        .send(models::tx_rollback::Request { id: tx.id })
        .await
        .expect("send failed")
        .expect("rollback failed");

    let event = tokio::time::timeout(Duration::from_secs(1), events_rx.recv_async()).await;
    assert!(event.is_err(), "rollback must not publish events");
}

#[tokio::test(flavor = "multi_thread")]
async fn namespace_subject_hears_every_scope_underneath() {
    let (session, _drop_tx) = start_node().await;
    let client = Client::new(&session);

    let (events_tx, events_rx) = flume::unbounded();
    let _sub = client
        .subscribe(
            Subject::Namespace(String::from("testing")),
            TABLE,
            move |notification| {
                let _ = events_tx.send(notification);
            },
        )
        .await
        .expect("unable to subscribe");

    let other = Scope::new("testing", "other", "private");

    let tx = client
        .send(models::tx_begin::Request::default())
        .await
        .expect("send failed")
        .expect("tx begin failed");

    insert_one(&client, tx.id, &scope()).await;
    insert_one(&client, tx.id, &other).await;

    client
        .send(models::tx_commit::Request { id: tx.id })
        .await
        .expect("send failed")
        .expect("commit failed");

    let mut seen = vec![];
    for _ in 0..2 {
        let notification = tokio::time::timeout(Duration::from_secs(10), events_rx.recv_async())
            .await
            .expect("no event within 10s")
            .expect("event channel closed");

        assert_eq!(notification.table, TABLE);
        seen.push(notification.scope);
    }

    seen.sort_by(|a, b| a.database.cmp(&b.database));
    assert_eq!(seen, vec![Scope::new("testing", "events", "public"), other]);
}

/// Reads one row of the test scope's table back, in a throwaway transaction.
async fn read_letter(client: &Client, eid: &[u8]) -> Option<Vec<u8>> {
    let tx = client
        .send(models::tx_begin::Request::default())
        .await
        .expect("send failed")
        .expect("tx begin failed");

    let row = client
        .send(models::tb_get::Request {
            id: tx.id,
            op: models::tb_get::Op {
                scope: scope(),
                table: TABLE.into(),
                eid: eid.to_vec(),
            },
        })
        .await
        .expect("send failed")
        .expect("get failed");

    client
        .send(models::tx_rollback::Request { id: tx.id })
        .await
        .expect("send failed")
        .expect("rollback failed");

    row.value
}

#[tokio::test(flavor = "multi_thread")]
async fn application_commits_ops_in_one_round_trip_and_publishes_one_event() {
    let (session, _drop_tx) = start_node().await;
    let client = Client::new(&session);

    let (events_tx, events_rx) = flume::unbounded();
    let _sub = client
        .subscribe(Subject::Scope(scope()), TABLE, move |notification| {
            let _ = events_tx.send(notification);
        })
        .await
        .expect("unable to subscribe");

    client
        .send(models::tx_apply::Request::commit_new(
            models::tx_begin::Constraint::Routed(scope()),
            vec![append(b"1", b"a"), append(b"2", b"b")],
        ))
        .await
        .expect("send failed")
        .expect("apply failed");

    assert_eq!(
        read_letter(&client, b"1").await.as_deref(),
        Some(b"a".as_slice())
    );
    assert_eq!(
        read_letter(&client, b"2").await.as_deref(),
        Some(b"b".as_slice())
    );

    let notification = tokio::time::timeout(Duration::from_secs(10), events_rx.recv_async())
        .await
        .expect("no event within 10s")
        .expect("event channel closed");

    assert_eq!(notification.scope, scope());
    assert_eq!(notification.table, TABLE);
    assert!(matches!(notification.event, TableEvent::Inserted(_)));

    // Both inserts landed in one commit, so there is exactly one poke.
    let extra = tokio::time::timeout(Duration::from_millis(500), events_rx.recv_async()).await;
    assert!(
        extra.is_err(),
        "expected a single event per table per commit"
    );
}

/// The tail rule end to end: deferred writes, a read that flushes them, then a
/// commit — all one transaction, and the read sees what has not committed yet.
#[tokio::test(flavor = "multi_thread")]
async fn a_kept_open_chain_reads_its_own_writes_and_commits_atomically() {
    let (session, _drop_tx) = start_node().await;
    let client = Client::new(&session);

    let opened = client
        .send(models::tx_apply::Request {
            target: models::tx_apply::Target::New {
                constraint: models::tx_begin::Constraint::Routed(scope()),
                access: models::tx_begin::Access::Write,
                retention_period: None,
            },
            ops: vec![append(b"1", b"a"), append(b"2", b"b")],
            finish: models::tx_apply::Finish::KeepOpen,
        })
        .await
        .expect("send failed")
        .expect("apply failed");

    let tx = opened.tx.expect("a kept-open application returns its tx");
    assert!(
        matches!(opened.last, Some(models::TxOpResponse::TbAppend(_))),
        "the reply carries the final op's response, whatever it is"
    );

    // Read-your-writes inside the chain, before anything committed.
    let read = client
        .send(models::tx_apply::Request {
            target: models::tx_apply::Target::Existing(tx),
            ops: vec![
                append(b"3", b"c"),
                models::TxOp::from(models::tb_count::Op {
                    scope: scope(),
                    table: TABLE.into(),
                }),
            ],
            finish: models::tx_apply::Finish::KeepOpen,
        })
        .await
        .expect("send failed")
        .expect("apply failed");

    let Some(models::TxOpResponse::TbCount(counted)) = read.last else {
        panic!("expected the tail count back");
    };
    assert_eq!(
        counted.count, 3,
        "the chain sees its own uncommitted writes"
    );

    // Nothing is visible outside the transaction until it commits.
    assert!(read_letter(&client, b"1").await.is_none());

    client
        .send(models::tx_apply::Request {
            target: models::tx_apply::Target::Existing(tx),
            ops: vec![],
            finish: models::tx_apply::Finish::Commit,
        })
        .await
        .expect("send failed")
        .expect("commit failed");

    for (eid, value) in [(b"1", b"a"), (b"2", b"b"), (b"3", b"c")] {
        assert_eq!(
            read_letter(&client, eid).await.as_deref(),
            Some(value.as_slice()),
            "every op in the chain committed together"
        );
    }
}

/// A failed op takes the whole transaction with it — there are no savepoints,
/// so a chain cannot be left half applied.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_op_rolls_back_the_whole_chain() {
    let (session, _drop_tx) = start_node().await;
    let client = Client::new(&session);

    let opened = client
        .send(models::tx_apply::Request {
            target: models::tx_apply::Target::New {
                constraint: models::tx_begin::Constraint::Routed(scope()),
                access: models::tx_begin::Access::Write,
                retention_period: None,
            },
            ops: vec![append(b"1", b"a")],
            finish: models::tx_apply::Finish::KeepOpen,
        })
        .await
        .expect("send failed")
        .expect("apply failed");

    let tx = opened.tx.expect("a kept-open application returns its tx");

    // A malformed SPARQL update is refused by the store, mid-batch.
    let err = client
        .send(models::tx_apply::Request {
            target: models::tx_apply::Target::Existing(tx),
            ops: vec![
                append(b"2", b"b"),
                models::TxOp::from(models::sem_update::Op {
                    scope: scope(),
                    query: String::from("this is not sparql"),
                    base_iri: None,
                }),
                append(b"3", b"c"),
            ],
            finish: models::tx_apply::Finish::Commit,
        })
        .await
        .expect("send failed")
        .expect_err("a malformed update must fail the application");

    assert_eq!(err.index, Some(1), "the error names the op that failed");

    // The transaction is gone, so a later application against it finds nothing
    // to apply to — the same silence a missing transaction has always answered
    // with, which the caller sees as an unanswered query.
    let orphaned = client
        .send(models::tx_apply::Request {
            target: models::tx_apply::Target::Existing(tx),
            ops: vec![append(b"4", b"d")],
            finish: models::tx_apply::Finish::Commit,
        })
        .await;
    assert!(
        orphaned.is_err(),
        "the transaction was rolled back and unregistered"
    );

    for eid in [b"1", b"2", b"3"] {
        assert!(
            read_letter(&client, eid).await.is_none(),
            "no op of a rolled-back chain survives"
        );
    }
}

/// One application may write several scopes; each committed scope pokes its own
/// subscribers exactly once.
#[tokio::test(flavor = "multi_thread")]
async fn one_application_writes_several_scopes() {
    let (session, _drop_tx) = start_node().await;
    let client = Client::new(&session);
    let other = Scope::new("testing", "events-other", "public");

    let (events_tx, events_rx) = flume::unbounded();
    let _sub = client
        .subscribe(
            Subject::Database("testing".into(), "events".into()),
            TABLE,
            {
                let events_tx = events_tx.clone();
                move |notification| {
                    let _ = events_tx.send(notification);
                }
            },
        )
        .await
        .expect("unable to subscribe");
    let _other_sub = client
        .subscribe(
            Subject::Database("testing".into(), "events-other".into()),
            TABLE,
            move |notification| {
                let _ = events_tx.send(notification);
            },
        )
        .await
        .expect("unable to subscribe");

    client
        .send(models::tx_apply::Request::commit_new(
            models::tx_begin::Constraint::Routed(scope()),
            vec![
                append(b"1", b"a"),
                models::TxOp::from(models::tb_append::Op {
                    scope: other.clone(),
                    table: TABLE.into(),
                    eid: Some(b"1".to_vec()),
                    value: b"z".to_vec(),
                }),
            ],
        ))
        .await
        .expect("send failed")
        .expect("apply failed");

    let mut seen = vec![];
    for _ in 0..2 {
        let notification = tokio::time::timeout(Duration::from_secs(10), events_rx.recv_async())
            .await
            .expect("no event within 10s")
            .expect("event channel closed");

        assert_eq!(notification.table, TABLE);
        seen.push(notification.scope);
    }

    seen.sort_by(|a, b| a.database.cmp(&b.database));
    assert_eq!(seen, vec![scope(), other]);
}

/// The client-side builder: deferred writes cost nothing until something needs
/// a value or the transaction commits.
#[tokio::test(flavor = "multi_thread")]
async fn an_application_defers_writes_until_it_commits() {
    let (session, _drop_tx) = start_node().await;
    let client = Client::new(&session);

    let mut application = Application::routed(Client::new(&session), scope());

    application
        .defer(models::tb_append::Op {
            scope: scope(),
            table: TABLE.into(),
            eid: Some(b"1".to_vec()),
            value: b"a".to_vec(),
        })
        .expect("defer failed");
    application
        .defer(models::tb_delete::Op {
            scope: scope(),
            table: TABLE.into(),
            eid: b"absent".to_vec(),
        })
        .expect("defer failed");

    // Nothing has been sent, so there is no transaction yet.
    assert!(application.tx().is_none(), "deferring opens no transaction");

    application.commit().await.expect("commit failed");

    assert_eq!(
        read_letter(&client, b"1").await.as_deref(),
        Some(b"a".as_slice())
    );
}

/// A read flushes what was deferred before it, in order, and sees it.
#[tokio::test(flavor = "multi_thread")]
async fn an_application_flushes_deferred_writes_before_a_read() {
    let (session, _drop_tx) = start_node().await;
    let client = Client::new(&session);

    let mut application = Application::routed(Client::new(&session), scope());

    application
        .defer(models::tb_append::Op {
            scope: scope(),
            table: TABLE.into(),
            eid: Some(b"1".to_vec()),
            value: b"a".to_vec(),
        })
        .expect("defer failed");

    let got = application
        .apply(models::tb_get::Op {
            scope: scope(),
            table: TABLE.into(),
            eid: b"1".to_vec(),
        })
        .await
        .expect("read failed");

    assert_eq!(
        got.value.as_deref(),
        Some(b"a".as_slice()),
        "the read sees the write deferred before it"
    );
    assert!(
        application.tx().is_some(),
        "the flush left the transaction open"
    );

    // Still uncommitted for everyone else.
    assert!(read_letter(&client, b"1").await.is_none());

    application.commit().await.expect("commit failed");

    assert_eq!(
        read_letter(&client, b"1").await.as_deref(),
        Some(b"a".as_slice())
    );
}

/// An application that never flushed has nothing to roll back, so abandoning it
/// touches the mesh not at all.
#[tokio::test(flavor = "multi_thread")]
async fn abandoning_an_unflushed_application_reaches_no_node() {
    let (session, _drop_tx) = start_node().await;
    let client = Client::new(&session);

    let mut application = Application::routed(Client::new(&session), scope());

    application
        .defer(models::tb_append::Op {
            scope: scope(),
            table: TABLE.into(),
            eid: Some(b"1".to_vec()),
            value: b"a".to_vec(),
        })
        .expect("defer failed");

    application.rollback().await.expect("rollback failed");

    assert!(read_letter(&client, b"1").await.is_none());
}

/// An insert whose id the caller supplies, as the deferrable form uses.
fn append(eid: &[u8], value: &[u8]) -> models::TxOp {
    models::TxOp::from(models::tb_append::Op {
        scope: scope(),
        table: TABLE.into(),
        eid: Some(eid.to_vec()),
        value: value.to_vec(),
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn one_shot_deletes_and_tb_peek_read_without_a_client_transaction() {
    let (session, _drop_tx) = start_node().await;
    let client = Client::new(&session);

    let eids: Vec<Vec<u8>> = (b'a'..=b'c').map(|letter| vec![letter]).collect();

    client
        .send(models::tx_apply::Request::commit_new(
            models::tx_begin::Constraint::Routed(scope()),
            eids.iter().map(|eid| append(eid, eid.as_slice())).collect(),
        ))
        .await
        .expect("send failed")
        .expect("apply failed");

    // A limited peek returns the head and the full count.
    let peeked = client
        .send(models::tb_peek::Request {
            scope: scope(),
            table: TABLE.into(),
            cursor: None,
            limit: Some(2),
            order: None,
            count: true,
        })
        .await
        .expect("send failed")
        .expect("peek failed");

    assert_eq!(peeked.count, Some(3));
    let head: Vec<_> = peeked.entities.iter().map(|(id, _)| id.clone()).collect();
    assert_eq!(head, eids[..2], "ascending id order from the head");

    // A cursor continues past what was already seen.
    let continued = client
        .send(models::tb_peek::Request {
            scope: scope(),
            table: TABLE.into(),
            cursor: Some(models::Cursor::After(eids[1].clone())),
            limit: None,
            order: None,
            count: false,
        })
        .await
        .expect("send failed")
        .expect("peek failed");

    assert_eq!(continued.count, None, "count only when asked for");
    assert_eq!(
        continued
            .entities
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>(),
        eids[2..],
    );

    // A one-shot delete is visible to the next peek.
    client
        .send(models::tx_apply::Request::commit_new(
            models::tx_begin::Constraint::Routed(scope()),
            vec![models::TxOp::from(models::tb_delete::Op {
                scope: scope(),
                table: TABLE.into(),
                eid: eids[0].clone(),
            })],
        ))
        .await
        .expect("send failed")
        .expect("apply failed");

    let after = client
        .send(models::tb_peek::Request {
            scope: scope(),
            table: TABLE.into(),
            cursor: None,
            limit: None,
            order: None,
            count: true,
        })
        .await
        .expect("send failed")
        .expect("peek failed");

    assert_eq!(after.count, Some(2));
    assert_eq!(
        after
            .entities
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>(),
        eids[1..],
    );
    assert!(read_letter(&client, &eids[0]).await.is_none());
}
