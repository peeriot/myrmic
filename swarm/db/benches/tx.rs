//! Transaction lifecycle overhead benches.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use db::domain::Scope;

mod common;

use common::{open_store, read_tx, runtime, write_tx};

fn empty_write_commit(c: &mut Criterion) {
    let rt = runtime();
    let store = rt.block_on(async { open_store() });

    c.bench_function("empty_write_commit", |b| {
        b.iter(|| {
            rt.block_on(async {
                let tx = write_tx(&store);
                tx.commit().expect("commit");
            });
        });
    });
}

fn empty_read_open(c: &mut Criterion) {
    let rt = runtime();
    let store = rt.block_on(async { open_store() });

    c.bench_function("empty_read_open", |b| {
        b.iter(|| {
            let tx = read_tx(&store);
            criterion::black_box(tx);
        });
    });
}

const BATCH_SIZES: &[usize] = &[1, 100];

fn batched_put(c: &mut Criterion) {
    let rt = runtime();
    let store = rt.block_on(async { open_store() });
    let scope = Scope::default();

    let mut group = c.benchmark_group("batched_put");
    for &n in BATCH_SIZES {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter(|| {
                rt.block_on(async {
                    let mut tx = write_tx(&store);
                    for i in 0..n {
                        let key = format!("b-{i}");
                        tx.key_put(
                            criterion::black_box(scope).kv(criterion::black_box(key.as_str())),
                            criterion::black_box(b"v"),
                        )
                        .expect("put");
                    }
                    tx.commit().expect("commit");
                });
            });
        });
    }
    group.finish();
}

criterion_group!(
    name = tx_benches;
    config = Criterion::default();
    targets = empty_write_commit, empty_read_open, batched_put
);
criterion_main!(tx_benches);
