//! Times moving a scope's data from one node to another the way
//! `myrmic replicate <scope> -t @target -e @source` does, sampling locate
//! throughout. Ignored; run on demand:
//!
//! ```text
//! locked cargo nextest run -p swarm handoff_timing --run-ignored only --no-capture
//! ```
//!
//! Sized by `HANDOFF_ROWS`, `HANDOFF_VALUE_BYTES` and `HANDOFF_ROWS_PER_TX`;
//! `HANDOFF_LOG` takes a tracing target filter. Ends by reading every row back
//! from the target, failing on any that went missing.

use std::time::{Duration, Instant};

use futures_util::StreamExt as _;
use rand::RngCore as _;

use super::{
    custody_key, locate_eventually, node_id, read_custody_row, start_connected_pair,
    start_connected_pair_with,
};
use cell_protocol::replication::{
    REPLICATION_TABLE, ReplicaEntry, ReplicaSelector, replication_scope, runtime_tag,
};
use db_client::v1::Client;
use db_commons::models::locate::HolderState;
use db_commons::models::{self, NodeId, Scope, Subject, Version, rendezvous_hash};

const PROBE_EVERY: Duration = Duration::from_millis(50);
const RUN_CAP: Duration = Duration::from_mins(10);
/// How long the source must stay out of locate, and silent on the scope's
/// replica channel, before its drain counts as retired. A drain announces at
/// least every 4s, and a hidden one never answers locate at all.
const SETTLE: Duration = Duration::from_secs(5);

pub(super) fn bench_scope() -> Scope {
    Scope::new("bench", "handoff", "public")
}

pub(super) struct Load {
    pub(super) rows: usize,
    pub(super) value_bytes: usize,
    pub(super) rows_per_tx: usize,
}

impl Load {
    fn from_env() -> Self {
        fn env_or(name: &str, default: usize) -> usize {
            std::env::var(name).map_or(default, |value| {
                value
                    .parse()
                    .unwrap_or_else(|_| panic!("{name} must be a number"))
            })
        }

        Self {
            rows: env_or("HANDOFF_ROWS", 10_000),
            value_bytes: env_or("HANDOFF_VALUE_BYTES", 256),
            rows_per_tx: env_or("HANDOFF_ROWS_PER_TX", 500).max(1),
        }
    }

    #[expect(clippy::cast_precision_loss, reason = "report arithmetic")]
    fn mib(&self) -> f64 {
        (self.rows * self.value_bytes) as f64 / (1024.0 * 1024.0)
    }
}

const ROWS_TABLE: &str = "rows";

fn digest(value: &[u8]) -> u64 {
    use std::hash::{Hash as _, Hasher as _};

    let mut hasher = std::hash::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// Commits the load into `scope`, `rows_per_tx` rows per transaction,
/// returning how long that took and each row's value digest, by row id.
pub(super) async fn insert_rows(
    client: &Client,
    scope: &Scope,
    load: &Load,
) -> (Duration, Vec<u64>) {
    // Random values, so storage compression doesn't flatter the transfer.
    let mut rng = rand::rng();
    let mut digests = Vec::with_capacity(load.rows);
    let start = Instant::now();

    for chunk_start in (0..load.rows).step_by(load.rows_per_tx) {
        let ops = (chunk_start..load.rows.min(chunk_start + load.rows_per_tx))
            .map(|row| {
                let mut value = vec![0; load.value_bytes];
                rng.fill_bytes(&mut value);
                digests.push(digest(&value));
                models::TxOp::from(models::tb_append::Op {
                    scope: scope.clone(),
                    table: ROWS_TABLE.into(),
                    eid: Some((row as u64).to_be_bytes().to_vec()),
                    value,
                })
            })
            .collect();

        client
            .send(models::tx_apply::Request::commit_new(
                models::tx_begin::Constraint::Routed(scope.clone()),
                ops,
            ))
            .await
            .expect("send failed")
            .expect("apply failed");
    }

    (start.elapsed(), digests)
}

/// What a read of the scope found against what was inserted.
struct Verified {
    landed: NodeId,
    read: usize,
    missing: Vec<u64>,
    corrupt: Vec<u64>,
}

/// Reads every row back the way any client would — a routed read, landing
/// wherever routing sends it.
async fn verify_read(client: &Client, scope: &Scope, digests: &[u64]) -> Verified {
    const PAGE: usize = 2000;

    let tx = client
        .send(models::tx_begin::Request {
            constraint: models::tx_begin::Constraint::Routed(scope.clone()),
            access: models::tx_begin::Access::Read,
            ..Default::default()
        })
        .await
        .expect("send failed")
        .expect("tx begin failed");

    let mut found = vec![None; digests.len()];
    let mut read = 0;
    let mut cursor = None;
    loop {
        let page = client
            .send(models::tb_list::Request {
                id: tx.id,
                op: models::tb_list::Op {
                    scope: scope.clone(),
                    table: ROWS_TABLE.into(),
                    cursor: cursor.take(),
                    limit: Some(PAGE),
                    order: None,
                },
            })
            .await
            .expect("send failed")
            .expect("list failed");

        read += page.entities.len();
        for (id, value) in &page.entities {
            let row = u64::from_be_bytes(id.as_slice().try_into().expect("an 8-byte row id"));
            if let Some(slot) = usize::try_from(row).ok().and_then(|row| found.get_mut(row)) {
                *slot = Some(digest(value));
            }
        }

        match page.entities.last() {
            Some((id, _)) if page.entities.len() == PAGE => {
                cursor = Some(models::Cursor::After(id.clone()));
            }
            _ => break,
        }
    }

    client
        .send(models::tx_rollback::Request { id: tx.id })
        .await
        .expect("send failed")
        .expect("rollback failed");

    let mut verified = Verified {
        landed: tx.id.2,
        read,
        missing: Vec::new(),
        corrupt: Vec::new(),
    };
    for (row, (want, got)) in (0u64..).zip(digests.iter().zip(&found)) {
        match got {
            None => verified.missing.push(row),
            Some(got) if got != want => verified.corrupt.push(row),
            Some(_) => (),
        }
    }
    verified
}

struct Holder {
    id: NodeId,
    head: Version,
    state: HolderState,
}

struct Sample {
    sent: Instant,
    first_reply: Option<Duration>,
    done: Duration,
    holders: Vec<Holder>,
}

/// Locates `scope` every [`PROBE_EVERY`] until the receiver hangs up.
async fn probe(replica: db_client::replica_v1::Client, scope: Scope, tx: flume::Sender<Sample>) {
    loop {
        let sent = Instant::now();
        let mut first_reply = None;
        let mut holders = Vec::new();

        let mut replies = Box::pin(
            replica
                .locate_stream(&scope, None)
                .await
                .expect("locate failed"),
        );
        while let Some(reply) = replies.next().await {
            first_reply.get_or_insert_with(|| sent.elapsed());
            holders.push(Holder {
                id: reply.id,
                head: reply.head,
                state: reply.state,
            });
        }

        let sample = Sample {
            sent,
            first_reply,
            done: sent.elapsed(),
            holders,
        };
        if tx.send(sample).is_err() {
            return;
        }

        tokio::time::sleep_until((sent + PROBE_EVERY).into()).await;
    }
}

/// Writes `entry` in a transaction placed on `node`, as the CLI's write is
/// when the routed draw picks that node. Placement can't be forced, so a
/// begin landing elsewhere is rolled back and retried.
async fn write_entry_on(client: &Client, node: NodeId, entry: &ReplicaEntry) {
    let deadline = Instant::now() + Duration::from_secs(30);

    loop {
        let tx = client
            .send(models::tx_begin::Request {
                constraint: models::tx_begin::Constraint::Routed(replication_scope()),
                ..Default::default()
            })
            .await
            .expect("send failed")
            .expect("tx begin failed");

        if tx.id.2 == node {
            client
                .send(models::tb_insert::Request {
                    id: tx.id,
                    op: models::tb_insert::Op {
                        scope: replication_scope(),
                        table: REPLICATION_TABLE.into(),
                        eid: Some(entry.key().into_bytes()),
                        value: postcard::to_allocvec(entry).expect("entry should serialise"),
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

            return;
        }

        client
            .send(models::tx_rollback::Request { id: tx.id })
            .await
            .expect("send failed")
            .expect("rollback failed");

        assert!(
            Instant::now() < deadline,
            "the replication entry never landed on the source"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// When `session` hears a replication-table event: what the membership
/// watcher on that node wakes on.
async fn hear_config_events(
    session: &zenoh::Session,
) -> (db_client::v1::Subscription, flume::Receiver<Instant>) {
    let (tx, rx) = flume::unbounded();
    let subscription = Client::new(session)
        .subscribe(
            Subject::Scope(replication_scope()),
            REPLICATION_TABLE,
            move |_| {
                let _ = tx.send(Instant::now());
            },
        )
        .await
        .expect("unable to subscribe");

    (subscription, rx)
}

/// When `session` hears `source` announce an offload of `scope` — the only
/// sign of a hidden drain, which never answers locate.
async fn hear_offload_announces(
    session: &zenoh::Session,
    source: &zenoh::Session,
    scope: &Scope,
) -> (zenoh::pubsub::Subscriber<()>, flume::Receiver<Instant>) {
    let (tx, rx) = flume::unbounded();
    let ke = db_commons::topics::replica::format_replica(
        &scope.namespace,
        &scope.database,
        &scope.schema,
        source.zid(),
        "*",
    );

    let scope = scope.clone();
    let subscriber = session
        .declare_subscriber(ke)
        .callback(move |sample| {
            let payload = sample.payload().to_bytes();
            if let Ok(models::ReplicaMessage::Announce(announce)) = postcard::from_bytes(&payload)
                && !announce.full_replica
                && announce.known.contains_key(&scope)
            {
                let _ = tx.send(Instant::now());
            }
        })
        .await
        .expect("unable to subscribe to the replica channel");

    (subscriber, rx)
}

type Node = (zenoh::Session, swarm_api::DropSender);

/// Orders a pair as (source, target). A `sys` write routes to the rendezvous
/// winner among equally caught-up holders, so making that node the source is
/// what lands the handoff write there — the target then only learns of it by
/// replication.
fn source_and_target((a, b): (Node, Node)) -> (Node, Node) {
    let draw = |session: &zenoh::Session| {
        let id = node_id(session);
        (rendezvous_hash(&replication_scope(), &id), id)
    };
    if draw(&a.0) > draw(&b.0) {
        (a, b)
    } else {
        (b, a)
    }
}

fn entry_for(session: &zenoh::Session) -> ReplicaEntry {
    let selector = ReplicaSelector::Subject(Subject::Scope(bench_scope()));
    let label = selector.to_string();
    ReplicaEntry::new(selector, vec![runtime_tag(session.zid().into())], &label)
}

/// The longest and total time a condition held across consecutive samples.
#[derive(Default)]
struct Gap {
    open: Option<Instant>,
    longest: Duration,
    total: Duration,
}

impl Gap {
    fn observe(&mut self, bad: bool, at: Instant) {
        match (bad, self.open) {
            (true, None) => self.open = Some(at),
            (false, Some(since)) => {
                self.close(at - since);
                self.open = None;
            }
            _ => (),
        }
    }

    fn finish(&mut self, at: Instant) {
        if let Some(since) = self.open.take() {
            self.close(at - since);
        }
    }

    fn close(&mut self, length: Duration) {
        self.longest = self.longest.max(length);
        self.total += length;
    }
}

/// What the probe saw from the handoff write (`t0`) on.
struct Watch {
    t0: Instant,
    source: NodeId,
    target: NodeId,
    source_head: Version,

    /// When the target's session heard the config write's table event.
    target_heard: Option<Instant>,
    source_draining: Option<Instant>,
    target_seen: Option<Instant>,
    target_caught_up: Option<Instant>,
    /// Start of the source's current absence from locate; reset if it answers
    /// again.
    source_gone: Option<Instant>,
    retired: bool,
    /// The source's offload announces for the scope: how many, and the last.
    offload_announces: usize,
    last_offload: Option<Instant>,
    /// Each change in how a node answers locate, with the last seen per node.
    transitions: Vec<(Instant, &'static str, String)>,
    views: [String; 2],

    no_holder: Gap,
    no_replica: Gap,
    none_at_head: Gap,
    first_replies: Vec<Duration>,
    completions: Vec<Duration>,
    empty: usize,
    last: Instant,
}

impl Watch {
    fn new(t0: Instant, source: NodeId, target: NodeId, source_head: Version) -> Self {
        Self {
            t0,
            source,
            target,
            source_head,
            target_heard: None,
            source_draining: None,
            target_seen: None,
            target_caught_up: None,
            source_gone: None,
            retired: false,
            offload_announces: 0,
            last_offload: None,
            transitions: Vec::new(),
            views: [String::new(), String::new()],
            no_holder: Gap::default(),
            no_replica: Gap::default(),
            none_at_head: Gap::default(),
            first_replies: Vec::new(),
            completions: Vec::new(),
            empty: 0,
            last: t0,
        }
    }

    /// Folds in one sample; true once the handoff has finished.
    fn observe(&mut self, sample: &Sample) -> bool {
        let at = sample.sent;
        if at < self.t0 {
            return false;
        }
        self.last = at;

        let holder = |id| sample.holders.iter().find(|holder| holder.id == id);
        let source = holder(self.source);
        let target = holder(self.target);

        if source.is_some_and(|holder| matches!(holder.state, HolderState::Draining)) {
            self.source_draining.get_or_insert(at);
        }
        if target.is_some() {
            self.target_seen.get_or_insert(at);
        }
        if target.is_some_and(|holder| holder.head >= self.source_head) {
            self.target_caught_up.get_or_insert(at);
        }
        for (slot, (name, holder)) in [("source", source), ("target", target)]
            .into_iter()
            .enumerate()
        {
            let view = match holder {
                None => String::from("absent"),
                Some(holder) if holder.head >= self.source_head => {
                    format!("{:?} at head", holder.state)
                }
                Some(holder) => format!("{:?} behind", holder.state),
            };
            if self.views[slot] != view {
                self.transitions.push((at, name, view.clone()));
                self.views[slot] = view;
            }
        }

        self.source_gone = match source {
            Some(_) => None,
            None => Some(self.source_gone.unwrap_or(at)),
        };

        let replica = |holder: &&Holder| matches!(holder.state, HolderState::Replica);
        self.no_holder.observe(sample.holders.is_empty(), at);
        self.no_replica
            .observe(!sample.holders.iter().any(|h| replica(&h)), at);
        self.none_at_head.observe(
            !sample
                .holders
                .iter()
                .any(|h| replica(&h) && h.head >= self.source_head),
            at,
        );

        match sample.first_reply {
            Some(first) => self.first_replies.push(first),
            None => self.empty += 1,
        }
        self.completions.push(sample.done);

        self.retired = self.target_caught_up.is_some()
            && self.source_gone.is_some_and(|since| at - since >= SETTLE)
            && self.last_offload.is_none_or(|last| at - last >= SETTLE);
        self.retired
    }

    fn finish(&mut self) {
        for gap in [
            &mut self.no_holder,
            &mut self.no_replica,
            &mut self.none_at_head,
        ] {
            gap.finish(self.last);
        }
    }

    fn print(&self, mib: f64) {
        println!("handoff (from the config write, t0)");
        let show = |label: &str, at: Option<Instant>| match at {
            Some(at) => println!("  {label:<14} {:.2?}", at - self.t0),
            None => println!("  {label:<14} never"),
        };
        show("target heard", self.target_heard);
        show("source drains", self.source_draining);
        show("target visible", self.target_seen);
        show("target at head", self.target_caught_up);
        show("source gone", self.source_gone.filter(|_| self.retired));
        show("last offload", self.last_offload);
        println!("  offload annc.  {}", self.offload_announces);
        if let (Some(seen), Some(caught_up)) = (self.target_seen, self.target_caught_up) {
            let took = caught_up - seen;
            let rate = mib / took.as_secs_f64().max(f64::EPSILON);
            println!("  transfer       {took:.2?} ({rate:.2} MiB/s)");
        }
        println!();

        println!("transitions (as locate answers)");
        for (at, name, view) in &self.transitions {
            println!("  {:>9.2?}  {name} {view}", *at - self.t0);
        }
        println!();

        println!("locate (every {PROBE_EVERY:?}, from the source's session)");
        print_latency("first reply", &self.first_replies);
        print_latency("complete", &self.completions);
        println!("  empty rounds   {}", self.empty);
        for (label, gap) in [
            ("no holder", &self.no_holder),
            ("no replica", &self.no_replica),
            ("none at head", &self.none_at_head),
        ] {
            println!(
                "  {label:<14} longest {:>9.2?} total {:>9.2?}",
                gap.longest, gap.total
            );
        }
    }
}

fn print_insert(load: &Load, elapsed: Duration, head: Version, config_write: Duration) {
    #[expect(clippy::cast_precision_loss, reason = "report arithmetic")]
    let rows_per_s = load.rows as f64 / elapsed.as_secs_f64();

    println!("insert");
    println!(
        "  rows           {} x {} B = {:.2} MiB, {} rows/tx",
        load.rows,
        load.value_bytes,
        load.mib(),
        load.rows_per_tx
    );
    println!("  elapsed        {elapsed:.2?} ({rows_per_s:.0} rows/s)");
    println!("  source head    {head}");
    println!("  config write   {config_write:.2?}");
    println!();
}

/// Routed counts of the scope, back to back for `window`: where each landed,
/// and every one that came back short.
#[derive(Default)]
struct Storm {
    reads: usize,
    on_source: usize,
    on_target: usize,
    short: Vec<(Duration, NodeId, usize)>,
    /// Every read: when it began, where it landed, what it counted.
    timeline: Vec<(Duration, NodeId, usize)>,
}

impl Storm {
    fn print(&self, window: Duration, source: NodeId) {
        if self.reads == 0 {
            return;
        }
        let name = |node: &NodeId| if *node == source { "source" } else { "target" };

        println!("read storm (routed counts for {window:?} from t0)");
        println!(
            "  reads          {} ({} on source, {} on target)",
            self.reads, self.on_source, self.on_target
        );
        println!("  short          {}", self.short.len());
        for (at, node, count) in &self.short {
            println!("    {at:>9.2?}  {} counted {count}", name(node));
        }
        println!("  timeline (first read of each second)");
        let mut second = None;
        for (at, node, count) in &self.timeline {
            if second != Some(at.as_secs()) {
                second = Some(at.as_secs());
                println!("    {at:>9.2?}  {} counted {count}", name(node));
            }
        }
        println!();
    }
}

async fn read_storm(
    client: &Client,
    scope: &Scope,
    rows: usize,
    source: NodeId,
    window: Duration,
) -> Storm {
    let mut storm = Storm::default();
    let start = Instant::now();

    while start.elapsed() < window {
        let began = start.elapsed();
        let (node, count) = routed_count(client, scope).await;

        storm.reads += 1;
        if node == source {
            storm.on_source += 1;
        } else {
            storm.on_target += 1;
        }
        if count < rows {
            storm.short.push((began, node, count));
        }
        storm.timeline.push((began, node, count));

        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    storm
}

/// One routed count of the scope in a single rolled-back application, as a
/// client would read it: the node that served it, and what it counted.
async fn routed_count(client: &Client, scope: &Scope) -> (NodeId, usize) {
    let applied = client
        .send(models::tx_apply::Request {
            target: models::tx_apply::Target::New {
                constraint: models::tx_begin::Constraint::Routed(scope.clone()),
                access: models::tx_begin::Access::Read,
                retention_period: None,
            },
            ops: vec![models::TxOp::from(models::tb_count::Op {
                scope: scope.clone(),
                table: ROWS_TABLE.into(),
            })],
            finish: models::tx_apply::Finish::Rollback,
        })
        .await
        .expect("send failed")
        .expect("count failed");

    let count = applied
        .last
        .map(models::tb_count::Response::try_from)
        .expect("a count was applied")
        .expect("the response is the count's")
        .count;

    (applied.node, count)
}

/// The read-back, with what locate said just before it: who answered, and
/// whom each responder vouched for.
fn print_verified(
    verified: &Verified,
    located: &[db_client::replica_v1::Located],
    source: NodeId,
    target: NodeId,
) {
    let name = |id: NodeId| match id {
        id if id == source => "source",
        id if id == target => "target",
        _ => "other",
    };
    let sample = |rows: &[u64]| rows.iter().take(10).copied().collect::<Vec<_>>();

    println!("read-back (a routed read, after the handoff)");
    for reply in located {
        println!("  answered       {} {:?}", name(reply.id), reply.state);
        for peer in &reply.peers {
            println!(
                "    vouches for  {} {:?}, heard {}ms ago",
                name(peer.id),
                peer.state,
                peer.age_ms
            );
        }
    }
    println!("  landed on      {}", name(verified.landed));
    println!("  rows read      {}", verified.read);
    println!(
        "  missing        {} {:?}",
        verified.missing.len(),
        sample(&verified.missing)
    );
    println!(
        "  corrupt        {} {:?}",
        verified.corrupt.len(),
        sample(&verified.corrupt)
    );
}

/// Logs to the test output when `HANDOFF_LOG` holds a target filter, e.g.
/// `swarm::plugins::db=info,db::replication=debug`.
fn init_logging() {
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    let Ok(filter) = std::env::var("HANDOFF_LOG") else {
        return;
    };
    let targets: tracing_subscriber::filter::Targets = format!("{filter},{}=info", module_path!())
        .parse()
        .expect("HANDOFF_LOG must be a target filter");

    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_test_writer()
                .with_timer(tracing_subscriber::fmt::time::uptime()),
        )
        .with(targets)
        .try_init();
}

fn print_latency(label: &str, values: &[Duration]) {
    let mut sorted = values.to_vec();
    sorted.sort();
    let percentile = |p: f64| {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "an index into a short sample list"
        )]
        let index = ((sorted.len().saturating_sub(1)) as f64 * p).round() as usize;
        sorted.get(index).copied().unwrap_or_default()
    };
    println!(
        "  {label:<14} n={:<6} p50={:>9.2?} p99={:>9.2?} max={:>9.2?}",
        sorted.len(),
        percentile(0.5),
        percentile(0.99),
        percentile(1.0),
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "timing harness; run on demand"]
async fn handoff_timing() {
    init_logging();
    let load = Load::from_env();
    let ((source, _source_drop), (target, _target_drop)) =
        source_and_target(start_connected_pair().await);
    let source_id = node_id(&source);
    let target_id = node_id(&target);

    let client = Client::new(&source);
    let scope = bench_scope();
    let replica = db_client::replica_v1::Client::new(&source, Subject::Scope(scope.clone()))
        .expect("unable to create replica client");

    write_entry_on(&client, source_id, &entry_for(&source)).await;
    locate_eventually(&replica, &scope, None).await;

    let (insert_elapsed, digests) = insert_rows(&client, &scope, &load).await;

    let pre = replica.locate(&scope, None).await.expect("locate failed");
    let source_head = pre
        .iter()
        .find(|holder| holder.id == source_id)
        .map(|holder| holder.head)
        .expect("the source should answer locate after the inserts");
    assert!(
        pre.iter().all(|holder| holder.id != target_id),
        "the target already held the scope before the handoff"
    );

    let (samples_tx, samples_rx) = flume::unbounded();
    let prober = tokio::spawn(probe(replica.clone(), scope.clone(), samples_tx));

    let (_heard, heard_rx) = hear_config_events(&target).await;
    let (_announced, announced_rx) = hear_offload_announces(&target, &source, &scope).await;

    let secs_from_env = |name| {
        std::env::var(name)
            .ok()
            .and_then(|secs| secs.parse().ok())
            .map_or(Duration::ZERO, Duration::from_secs)
    };
    let storm_window = secs_from_env("HANDOFF_READ_STORM_SECS");

    let t0 = Instant::now();
    tracing::info!("handoff t0");
    write_entry_on(&client, source_id, &entry_for(&target)).await;
    let config_write = t0.elapsed();

    // Routed reads throughout the handoff, as any client would make them.
    let storm = tokio::spawn({
        let (client, scope) = (client.clone(), scope.clone());
        let rows = load.rows;
        async move { read_storm(&client, &scope, rows, source_id, storm_window).await }
    });

    let mut watch = Watch::new(t0, source_id, target_id, source_head);
    while t0.elapsed() < RUN_CAP {
        let Ok(sample) = samples_rx.recv_async().await else {
            break;
        };
        for at in announced_rx.try_iter().filter(|at| *at >= t0) {
            watch.offload_announces += 1;
            watch.last_offload = Some(at);
        }
        if watch.observe(&sample) {
            break;
        }
    }
    drop(samples_rx);
    prober.abort();
    watch.target_heard = heard_rx.try_iter().find(|at| *at >= t0);
    watch.finish();

    let storm = storm.await.expect("the read storm panicked");

    // Long enough to see what the source does with its copy afterwards.
    tokio::time::sleep(secs_from_env("HANDOFF_LINGER_SECS")).await;
    tracing::info!("handoff read-back");

    let located = replica.locate(&scope, None).await.expect("locate failed");
    let verified = verify_read(&client, &scope, &digests).await;

    println!();
    println!("== handoff timing ==");
    println!("  source {source_id:02x?}");
    println!("  target {target_id:02x?}");
    println!();
    print_insert(&load, insert_elapsed, source_head, config_write);
    watch.print(load.mib());
    println!();
    print_verified(&verified, &located, source_id, target_id);
    println!();
    storm.print(storm_window, source_id);

    assert!(
        verified.missing.is_empty() && verified.corrupt.is_empty(),
        "a read after the handoff is missing or corrupting rows"
    );
    assert!(
        storm.short.is_empty(),
        "{} routed read(s) during the handoff came back short",
        storm.short.len()
    );
    assert!(
        watch.retired,
        "the handoff did not finish within {RUN_CAP:?}"
    );
}

/// A drain whose data a replica is pulling is not stranded, however long the
/// pull runs: escalation counts from the last fetch, not from the drain's
/// start. Escalating would promote the source into a custodian of the very
/// scope it is handing over.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "depends on the pull outlasting a 2s timeout; run on demand"]
async fn a_drain_being_pulled_from_does_not_escalate() {
    init_logging();
    // Far shorter than the pull below, far longer than a gap between pages —
    // including the occasional page that stalls for a second or so.
    let config = super::super::config::Config {
        store: super::super::config::StoreConfig {
            offload_escalation_timeout: Some(Duration::from_secs(2)),
            ..Default::default()
        },
        ..Default::default()
    };
    let ((source, _source_drop), (target, _target_drop)) =
        source_and_target(start_connected_pair_with(&config).await);
    let (source_id, target_id) = (node_id(&source), node_id(&target));

    let client = Client::new(&source);
    let scope = bench_scope();
    let replica = db_client::replica_v1::Client::new(&source, Subject::Scope(scope.clone()))
        .expect("unable to create replica client");

    write_entry_on(&client, source_id, &entry_for(&source)).await;
    locate_eventually(&replica, &scope, None).await;

    let load = Load {
        rows: 100_000,
        value_bytes: 256,
        rows_per_tx: 500,
    };
    insert_rows(&client, &scope, &load).await;
    let head = replica
        .locate(&scope, None)
        .await
        .expect("locate failed")
        .iter()
        .find(|holder| holder.id == source_id)
        .map(|holder| holder.head)
        .expect("the source should answer locate");

    let started = Instant::now();
    write_entry_on(&client, source_id, &entry_for(&target)).await;

    // Pulled over: the target answers at the source's head.
    let holders = loop {
        let holders = replica.locate(&scope, None).await.expect("locate failed");
        if holders
            .iter()
            .any(|holder| holder.id == target_id && holder.head >= head)
        {
            break holders;
        }
        assert!(
            started.elapsed() < Duration::from_mins(1),
            "the pull did not finish"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(
        started.elapsed() > Duration::from_secs(3),
        "the pull must outlast the escalation timeout for this to test anything"
    );

    // The pull ran many timeouts long, and the source never took the scope
    // back. (Retiring afterwards waits on an announce, which the production
    // timeout covers and this one does not, so that part is not tested.)
    assert!(
        holders
            .iter()
            .all(|holder| holder.id != source_id || matches!(holder.state, HolderState::Draining)),
        "the source escalated back into a replica mid-pull"
    );
    let custody = read_custody_row(&client, &custody_key(&source, &scope)).await;
    assert!(
        custody.is_none(),
        "the source escalated into a custodian mid-pull"
    );
}
