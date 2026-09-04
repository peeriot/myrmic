//! Worker mode: hosts one db node, takes commands from the orchestrator,
//! and exchanges `ReplicaMessage`s through it. Killed by SIGKILL mid-flight;
//! never expects a graceful shutdown unless told.

use crate::proto::{self, Op, ToParent, ToWorker, TxSpec};
use db::domain;
use db::domain::Subject;
use db::replication::ReplicaTransport;
use db::store::Options;
use db::store::{Store, TransactionMode, TransactionOptions};
use db_commons::models::ReplicaMessage;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::sync::mpsc;

#[derive(Clone)]
struct UdsTransport {
    out: mpsc::UnboundedSender<ToParent>,
}

impl ReplicaTransport for UdsTransport {
    fn publish(&self, msg: ReplicaMessage) -> impl Future<Output = ()> + Send {
        let payload = postcard::to_allocvec(&msg).expect("unable to encode replica message");
        let _ = self.out.send(ToParent::Replica { payload });
        std::future::ready(())
    }
}

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("missing worker env {key}"))
}

pub async fn run() -> anyhow::Result<()> {
    let name = env(proto::env::NAME);
    let socket = env(proto::env::SOCKET);
    let dir = env(proto::env::DIR);
    let namespace = env(proto::env::NAMESPACE);
    let gc_ms = std::env::var(proto::env::GC_MS)
        .ok()
        .map(|v| v.parse::<u64>().expect("bad GC_MS"));

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    let hlc = Arc::new(uhlc::HLC::default());
    let node_id = hlc.get_id().to_le_bytes();

    let store: Store = Store::init(Options {
        directory: Some(dir.into()),
        logic_clock: hlc,
        gc_interval: gc_ms.map(Duration::from_millis),
    })?;

    let stream = UnixStream::connect(&socket).await?;
    let (mut read_half, mut write_half) = stream.into_split();

    // Single writer task so frames from concurrent tasks never interleave.
    let (out, mut out_rx) = mpsc::unbounded_channel::<ToParent>();
    tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            if proto::write_frame(&mut write_half, &frame).await.is_err() {
                // Parent gone — nothing sensible left to do.
                std::process::exit(0);
            }
        }
    });

    out.send(ToParent::Hello {
        name: name.clone(),
        node_id,
        pid: std::process::id(),
    })?;

    let subject = Subject::Namespace(namespace);
    let transport = UdsTransport { out: out.clone() };
    let replicator = store
        .replicate(transport, subject.clone())
        .expect("replicate should start for a fresh store");

    // A booting node announces what it has, like a freshly joined peer.
    replicator.announce().await?;

    loop {
        let msg: ToWorker = match proto::read_frame(&mut read_half).await {
            Ok(msg) => msg,
            // Parent closed the socket (orchestrator exit) — just stop.
            Err(_) => return Ok(()),
        };

        match msg {
            ToWorker::RunTx(spec) => {
                let store = store.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let _ = out.send(run_tx(&store, &spec));
                });
            }
            ToWorker::Dump { scope } => {
                let store = store.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let entries = dump(&store, &scope).expect("dump failed");
                    let _ = out.send(ToParent::DumpResult { scope, entries });
                });
            }
            ToWorker::Heads => {
                let store = store.clone();
                let subject = subject.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let heads = heads(&store, &subject).expect("heads failed");
                    let _ = out.send(ToParent::HeadsResult { heads });
                });
            }
            ToWorker::Announce => {
                let replicator = replicator.clone();
                tokio::spawn(async move {
                    if let Err(err) = replicator.announce().await {
                        tracing::error!("announce failed: {err}");
                    }
                });
            }
            ToWorker::Replica { from, payload } => {
                let msg: ReplicaMessage = postcard::from_bytes(&payload)?;
                let sender = uhlc::ID::try_from(from).expect("zero sender id");
                // Production spawns each incoming message the same way.
                tokio::spawn(replicator.clone().handle_message(sender, msg));
            }
            ToWorker::Shutdown => {
                let _ = out.send(ToParent::Done);
                // Give the writer task a beat to flush the ack.
                tokio::time::sleep(Duration::from_millis(50)).await;
                return Ok(());
            }
        }
    }
}

fn run_tx(store: &Store, spec: &TxSpec) -> ToParent {
    let id = spec.id;

    let commit = || {
        let opts = match spec.retention_ms {
            Some(ms) => TransactionOptions::retain_for(
                TransactionMode::ReadWrite,
                Duration::from_millis(ms),
            ),
            None => TransactionOptions::write(),
        };

        let mut tx = store.begin_local(&opts)?;
        let ts = tx.timestamp().get_time().as_u64();

        for op in &spec.ops {
            let scope = op.scope();
            let dscope = domain::Key::new_scope(&scope.namespace, &scope.database, &scope.schema);

            match op {
                Op::Put { key, payload, .. } => {
                    // The value embeds (tx id, version ts) so any value observed
                    // later identifies its writing transaction and store version.
                    let value = format!("{id}:{ts}:{payload}");
                    tx.key_put(dscope.kv(key), value.as_bytes())?;
                }
                Op::Del { key, .. } => {
                    tx.key_delete(dscope.kv(key))?;
                }
            }
        }

        tx.commit()?;
        anyhow::Ok(ts)
    };

    match commit() {
        Ok(ts) => ToParent::TxResult {
            id,
            ts,
            ok: true,
            error: None,
        },
        Err(err) => ToParent::TxResult {
            id,
            ts: 0,
            ok: false,
            error: Some(format!("{err:#}")),
        },
    }
}

fn dump(store: &Store, scope: &db_commons::models::Scope) -> anyhow::Result<Vec<(String, String)>> {
    let mut tx = store.begin_local(&TransactionOptions::read())?;
    let dscope = domain::Key::new_scope(&scope.namespace, &scope.database, &scope.schema);

    let keys = tx.key_prefix(dscope, "")?;
    let mut entries = Vec::with_capacity(keys.len());
    for key in keys {
        if let Some(value) = tx.key_get(dscope.kv(&key))? {
            entries.push((key, String::from_utf8_lossy(&value).into_owned()));
        }
    }

    Ok(entries)
}

fn heads(store: &Store, subject: &Subject) -> anyhow::Result<Vec<proto::HeadEntry>> {
    let tx = store.begin_local(&TransactionOptions::read())?;
    let (lower, upper) = domain::SyncPoint::range_from_subject(subject)?;

    let mut heads = vec![];
    tx.collect_latest_heads(lower, upper, |scope, (epoch, ts, node), sm| {
        heads.push(proto::HeadEntry {
            scope,
            epoch,
            ts,
            node,
            deletion: matches!(sm.marker, domain::SyncMarker::Deletion),
        });
        Ok(())
    })?;

    Ok(heads)
}
