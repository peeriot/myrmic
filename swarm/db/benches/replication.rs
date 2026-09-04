//! Replication apply-path bench: feed `ChangeSet` messages through `Replicator::handle_message`.

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use db::domain::Subject;
use db::replication::ReplicaTransport;
use db_commons::models::replication::{ChangeSet, Chunk, SyncMarker, SyncMeta};
use db_commons::models::{ReplicaMessage, Scope as ApiScope};
use std::future::Future;
use std::sync::{Arc, Mutex};

mod common;

use common::{open_store, runtime};

#[derive(Clone, Default)]
struct NullTransport {
    sink: Arc<Mutex<Vec<ReplicaMessage>>>,
}

impl ReplicaTransport for NullTransport {
    fn publish(&self, msg: ReplicaMessage) -> impl Future<Output = ()> + Send {
        self.sink.lock().expect("lock").push(msg);
        std::future::ready(())
    }
}

const CHANGESET_SIZES: &[usize] = &[100, 10_000];

fn build_changeset(scope: &ApiScope, sender: uhlc::ID, n: usize) -> ChangeSet {
    let entries: Vec<(Vec<u8>, Option<Vec<u8>>)> = (0..n as u64)
        .map(|i| {
            let mut k = b"bench-".to_vec();
            k.extend_from_slice(&i.to_be_bytes());
            (k, Some(vec![0u8; 16]))
        })
        .collect();

    // SyncPointId is (Epoch, Version, NodeId) — (u64, [u8; 16]).
    let id = (0, n as u64, sender.to_le_bytes());

    ChangeSet {
        tx_id: None,
        scope: scope.clone(),
        chunks: vec![Chunk {
            id,
            meta: SyncMeta {
                parent: None,
                parent_epoch: None,
                marker: SyncMarker::Mutation,
                retention_period: None,
            },
            entries,
        }],
    }
}

fn apply_changeset(c: &mut Criterion) {
    let rt = runtime();
    let subject = Subject::Namespace("bench".to_string());
    let scope = ApiScope::new("bench", "db", "schema");

    let mut group = c.benchmark_group("apply_changeset");
    for &n in CHANGESET_SIZES {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_batched(
                || {
                    // `replicate` calls `tokio::spawn` internally, so everything must
                    // run inside the runtime context.
                    rt.block_on(async {
                        let store = open_store();
                        let transport = NullTransport::default();
                        let replicator = store
                            .replicate(transport.clone(), subject.clone())
                            .expect("replicate");
                        let sender = uhlc::ID::rand();
                        let cs = build_changeset(&scope, sender, n);
                        (store, transport, replicator, sender, cs)
                    })
                },
                |(store, _transport, replicator, sender, cs)| {
                    rt.block_on(async {
                        replicator
                            .clone()
                            .handle_message(
                                criterion::black_box(sender),
                                criterion::black_box(ReplicaMessage::ChangeSet(cs)),
                            )
                            .await;
                    });
                    drop(store);
                },
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}

criterion_group!(
    name = replication_benches;
    config = Criterion::default();
    targets = apply_changeset
);
criterion_main!(replication_benches);
