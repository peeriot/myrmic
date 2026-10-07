//! How fast committed data reaches a scope's other replicas. Ignored; run on
//! demand:
//!
//! ```text
//! locked cargo nextest run -p swarm replication_speed --run-ignored only --no-capture
//! ```
//!
//! Three nodes, three measurements:
//!
//! - catch-up: a replica added to a scope already holding `REPL_ROWS` rows,
//!   timed until it holds the newest;
//! - propagation: single-row commits spaced out, each timed until every other
//!   replica holds it;
//! - sustained: commits at rising rates, sampled for lag throughout, then
//!   timed until the replicas drain the backlog.
//!
//! Every wait is measured on this process's clock alone: a replica counts as
//! holding a version once it answers a locate at exactly that version.
//! Subtracting a writer's version stamp from another node's clock is not
//! trustworthy across hosts.

use std::time::{Duration, Instant};

use super::handoff_timing::{Load, bench_scope, insert_rows};
use super::{locate_eventually, node_id, start_node_on};
use cell_protocol::replication::{
    REPLICATION_TABLE, ReplicaEntry, ReplicaSelector, replication_scope, runtime_tag,
};
use db_client::v1::Client;
use db_commons::models::{self, NodeId, Scope, Subject, Version};

const NODES: usize = 3;
const PROPAGATION_COMMITS: usize = 30;
const PROPAGATION_SPACING: Duration = Duration::from_millis(300);
const SUSTAINED_RATES: [u32; 3] = [20, 100, 400];
const SUSTAINED_FOR: Duration = Duration::from_secs(10);
const SAMPLE_EVERY: Duration = Duration::from_millis(250);
/// How long a version may take to reach every replica before it counts as
/// never arriving.
const ARRIVAL_CAP: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(5);

type Node = (zenoh::Session, swarm_api::DropSender);

/// `n` nodes, every one connected to the first.
async fn start_mesh(n: usize) -> Vec<Node> {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("unable to bind a loopback port")
        .local_addr()
        .expect("no local address")
        .port();
    let endpoints = format!(r#"["tcp/127.0.0.1:{port}"]"#);

    let mut nodes = vec![
        start_node_on(
            |zenoh| zenoh.insert_json5("listen/endpoints", &endpoints).unwrap(),
            Default::default(),
        )
        .await,
    ];
    for _ in 1..n {
        nodes.push(
            start_node_on(
                |zenoh| zenoh.insert_json5("connect/endpoints", &endpoints).unwrap(),
                Default::default(),
            )
            .await,
        );
    }
    nodes
}

/// Configures `scope` onto exactly the nodes behind `sessions`.
async fn replicate_onto(client: &Client, scope: &Scope, sessions: &[&zenoh::Session]) {
    let selector = ReplicaSelector::Subject(Subject::Scope(scope.clone()));
    let label = selector.to_string();
    let tags = sessions
        .iter()
        .map(|session| runtime_tag(session.zid().into()))
        .collect();
    let entry = ReplicaEntry::new(selector, tags, &label);
    let value = postcard::to_allocvec(&entry).expect("entry should serialise");

    client
        .write_tx_in(replication_scope(), async move |client, tx_id| {
            client
                .send(models::tb_insert::Request {
                    id: tx_id,
                    op: models::tb_insert::Op {
                        scope: replication_scope(),
                        table: REPLICATION_TABLE.into(),
                        eid: Some(entry.key().into_bytes()),
                        value,
                    },
                })
                .await?
                .map_err(|err| format!("insert failed: {}", err.message))?;
            Ok(())
        })
        .await
        .expect("unable to write the replication entry");
}

/// The nodes that hold `version` of `scope` right now.
async fn holding(
    replica: &db_client::replica_v1::Client,
    scope: &Scope,
    version: Version,
) -> Vec<NodeId> {
    replica
        .locate(scope, Some(version))
        .await
        .map(|holders| holders.into_iter().map(|holder| holder.id).collect())
        .unwrap_or_default()
}

/// When each of `nodes` first held `version`, polled from `started`; `None`
/// for one that did not within [`ARRIVAL_CAP`].
async fn arrivals(
    replica: &db_client::replica_v1::Client,
    scope: &Scope,
    version: Version,
    nodes: &[NodeId],
    started: Instant,
) -> Vec<Option<Duration>> {
    let mut arrived = vec![None; nodes.len()];
    while arrived.iter().any(Option::is_none) && started.elapsed() < ARRIVAL_CAP {
        let now = holding(replica, scope, version).await;
        for (slot, node) in arrived.iter_mut().zip(nodes) {
            if slot.is_none() && now.contains(node) {
                *slot = Some(started.elapsed());
            }
        }
        tokio::time::sleep(POLL).await;
    }
    arrived
}

/// One single-row commit, routed as any client's would be: the node it
/// landed on and the version it committed at.
async fn commit_one(
    client: &Client,
    scope: &Scope,
    row: u64,
    events: &flume::Receiver<models::events::Notification>,
) -> (NodeId, Version) {
    let tx = client
        .send(models::tx_begin::Request {
            constraint: models::tx_begin::Constraint::Routed(scope.clone()),
            ..Default::default()
        })
        .await
        .expect("send failed")
        .expect("tx begin failed");
    client
        .send(models::tb_insert::Request {
            id: tx.id,
            op: models::tb_insert::Op {
                scope: scope.clone(),
                table: "live".into(),
                eid: Some(row.to_be_bytes().to_vec()),
                value: vec![0; 64],
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

    let event = tokio::time::timeout(Duration::from_secs(10), events.recv_async())
        .await
        .expect("no table event within 10s")
        .expect("event channel closed");
    (tx.id.2, event.version())
}

struct Lags {
    values: Vec<Duration>,
    lost: usize,
}

impl Lags {
    fn new() -> Self {
        Self {
            values: Vec::new(),
            lost: 0,
        }
    }

    fn add(&mut self, arrivals: &[Option<Duration>]) {
        for arrival in arrivals {
            match arrival {
                Some(lag) => self.values.push(*lag),
                None => self.lost += 1,
            }
        }
    }

    fn line(&self) -> String {
        let mut sorted = self.values.clone();
        sorted.sort();
        let at = |p: f64| {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss,
                reason = "an index into a short sample list"
            )]
            let index = (sorted.len().saturating_sub(1) as f64 * p).round() as usize;
            sorted.get(index).copied().unwrap_or_default()
        };
        format!(
            "n={:<4} p50={:>9.2?} p90={:>9.2?} p99={:>9.2?} max={:>9.2?} never={}",
            sorted.len(),
            at(0.5),
            at(0.9),
            at(0.99),
            at(1.0),
            self.lost
        )
    }
}

/// Everything in the mesh but `writer`.
fn others(ids: &[NodeId], writer: NodeId) -> Vec<NodeId> {
    ids.iter().copied().filter(|id| *id != writer).collect()
}

async fn measure_catch_up(nodes: &[Node], client: &Client, scope: &Scope) {
    let rows = std::env::var("REPL_ROWS")
        .ok()
        .and_then(|rows| rows.parse().ok())
        .unwrap_or(20_000);
    let load = Load {
        rows,
        value_bytes: 256,
        rows_per_tx: 500,
    };
    let replica = db_client::replica_v1::Client::new(&nodes[0].0, Subject::Scope(scope.clone()))
        .expect("unable to create replica client");

    replicate_onto(client, scope, &[&nodes[0].0, &nodes[1].0]).await;
    locate_eventually(&replica, scope, None).await;
    insert_rows(client, scope, &load).await;

    let head = replica
        .locate(scope, None)
        .await
        .expect("locate failed")
        .iter()
        .map(|holder| holder.head)
        .max()
        .expect("someone holds the scope");

    let started = Instant::now();
    replicate_onto(client, scope, &[&nodes[0].0, &nodes[1].0, &nodes[2].0]).await;
    let joiner = node_id(&nodes[2].0);
    let arrived = arrivals(&replica, scope, head, &[joiner], started).await;

    println!("catch-up ({rows} rows x 256 B onto a new replica)");
    match arrived[0] {
        Some(took) => println!("  newest version held after {took:.2?}"),
        None => println!("  newest version never arrived within {ARRIVAL_CAP:?}"),
    }
}

async fn measure_propagation(
    client: &Client,
    replica: &db_client::replica_v1::Client,
    scope: &Scope,
    ids: &[NodeId],
    events: &flume::Receiver<models::events::Notification>,
) {
    let mut lags = Lags::new();
    for row in 0..PROPAGATION_COMMITS {
        let committed = Instant::now();
        let (writer, version) = commit_one(client, scope, row as u64, events).await;
        let arrived = arrivals(replica, scope, version, &others(ids, writer), committed).await;
        lags.add(&arrived);
        tokio::time::sleep(PROPAGATION_SPACING.saturating_sub(committed.elapsed())).await;
    }

    println!(
        "propagation ({PROPAGATION_COMMITS} single-row commits, {PROPAGATION_SPACING:?} apart)"
    );
    println!("  commit -> held by each other replica: {}", lags.line());
}

async fn measure_sustained(
    client: &Client,
    replica: &db_client::replica_v1::Client,
    scope: &Scope,
    ids: &[NodeId],
    events: &flume::Receiver<models::events::Notification>,
) {
    println!("sustained (single-row commits for {SUSTAINED_FOR:?} per rate)");
    let mut row = 1_000_000u64;

    for rate in SUSTAINED_RATES {
        let spacing = Duration::from_secs(1) / rate;
        let started = Instant::now();
        let mut next_sample = started;
        let mut samples = Vec::new();
        let mut last = None;
        let mut commits = 0u32;

        while started.elapsed() < SUSTAINED_FOR {
            let committed = Instant::now();
            let (writer, version) = commit_one(client, scope, row, events).await;
            row += 1;
            commits += 1;
            last = Some((writer, version));

            if committed >= next_sample {
                next_sample = committed + SAMPLE_EVERY;
                let (replica, scope, others) =
                    (replica.clone(), scope.clone(), others(ids, writer));
                samples.push(tokio::spawn(async move {
                    arrivals(&replica, &scope, version, &others, committed).await
                }));
            }
            tokio::time::sleep(spacing.saturating_sub(committed.elapsed())).await;
        }
        let wrote_for = started.elapsed();

        let (writer, version) = last.expect("at least one commit");
        let stopped = Instant::now();
        let drained = arrivals(replica, scope, version, &others(ids, writer), stopped).await;

        let mut lags = Lags::new();
        for sample in samples {
            lags.add(&sample.await.expect("a lag sample panicked"));
        }

        let achieved = f64::from(commits) / wrote_for.as_secs_f64();
        let drain = drained
            .iter()
            .copied()
            .collect::<Option<Vec<_>>>()
            .and_then(|all| all.into_iter().max());
        println!(
            "  {rate:>4}/s asked, {achieved:>6.1}/s achieved: {}",
            lags.line()
        );
        match drain {
            Some(took) => {
                println!("        last commit held everywhere {took:.2?} after writes stopped");
            }
            None => println!("        last commit not held everywhere within {ARRIVAL_CAP:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "timing harness; run on demand"]
async fn replication_speed() {
    let nodes = start_mesh(NODES).await;
    let ids: Vec<NodeId> = nodes.iter().map(|(session, _)| node_id(session)).collect();
    let client = Client::new(&nodes[0].0);
    let scope = bench_scope();

    println!();
    println!("== replication speed ({NODES} nodes) ==");
    measure_catch_up(&nodes, &client, &scope).await;

    let replica = db_client::replica_v1::Client::new(&nodes[0].0, Subject::Scope(scope.clone()))
        .expect("unable to create replica client");
    let (events_tx, events) = flume::unbounded();
    let _sub = client
        .subscribe(Subject::Scope(scope.clone()), "live", move |notification| {
            let _ = events_tx.send(notification);
        })
        .await
        .expect("unable to subscribe");

    measure_propagation(&client, &replica, &scope, &ids, &events).await;
    measure_sustained(&client, &replica, &scope, &ids, &events).await;
    println!();
}
