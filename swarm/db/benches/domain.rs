//! Domain-layer benches: user kv, tables, blobs, timeseries.

use std::sync::atomic::{AtomicU64, Ordering};

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use db::domain::Scope;

mod common;

use common::{open_store, random_value, runtime, seeded_rng, write_tx};

// ---------- user kv ----------

const KV_VALUE_SIZES: &[usize] = &[64, 1 << 10, 64 << 10]; // 64 B, 1 KiB, 64 KiB

fn kv_put(c: &mut Criterion) {
    let rt = runtime();
    let store = rt.block_on(async { open_store() });
    let scope = Scope::default();
    let mut rng = seeded_rng();
    let counter = AtomicU64::new(0);

    let mut group = c.benchmark_group("kv_put");
    for &value_size in KV_VALUE_SIZES {
        let value = random_value(&mut rng, value_size);
        group.bench_with_input(
            BenchmarkId::from_parameter(value_size),
            &value_size,
            |b, _| {
                b.iter_batched(
                    || counter.fetch_add(1, Ordering::Relaxed),
                    |i| {
                        rt.block_on(async {
                            let mut tx = write_tx(&store);
                            let key = format!("k-{i}");
                            tx.key_put(
                                criterion::black_box(scope).kv(criterion::black_box(key.as_str())),
                                criterion::black_box(&value),
                            )
                            .expect("put");
                            tx.commit().expect("commit");
                        });
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

fn kv_get(c: &mut Criterion) {
    let rt = runtime();
    let store = rt.block_on(async { open_store() });
    let scope = Scope::default();
    let mut rng = seeded_rng();

    let mut group = c.benchmark_group("kv_get");
    for &value_size in KV_VALUE_SIZES {
        let value = random_value(&mut rng, value_size);

        // Seed one key we read repeatedly.
        rt.block_on(async {
            let mut tx = write_tx(&store);
            tx.key_put(scope.kv("hot"), &value).expect("seed put");
            tx.commit().expect("seed commit");
        });

        group.bench_with_input(
            BenchmarkId::from_parameter(value_size),
            &value_size,
            |b, _| {
                b.iter(|| {
                    let mut tx = common::read_tx(&store);
                    let out = tx
                        .key_get(criterion::black_box(scope).kv(criterion::black_box("hot")))
                        .expect("get");
                    criterion::black_box(out);
                });
            },
        );
    }
    group.finish();
}

fn kv_delete(c: &mut Criterion) {
    let rt = runtime();
    let store = rt.block_on(async { open_store() });
    let scope = Scope::default();

    c.bench_function("kv_delete", |b| {
        b.iter_batched(
            || {
                // Seed a fresh key per iteration so delete actually has something to remove.
                let id = uuid::Uuid::new_v4();
                let key = format!("d-{id}");
                rt.block_on(async {
                    let mut tx = write_tx(&store);
                    tx.key_put(scope.kv(&key), b"x").expect("seed put");
                    tx.commit().expect("seed commit");
                });
                key
            },
            |key| {
                rt.block_on(async {
                    let mut tx = write_tx(&store);
                    tx.key_delete(
                        criterion::black_box(scope).kv(criterion::black_box(key.as_str())),
                    )
                    .expect("delete");
                    tx.commit().expect("commit");
                });
            },
            BatchSize::SmallInput,
        );
    });
}

// ---------- tables ----------

const TB_READ_ROW_COUNTS: &[usize] = &[100, 10_000];

fn seed_table(rt: &tokio::runtime::Runtime, store: &db::store::Store, rows: usize) {
    let scope = Scope::default();
    let table = scope.table("rows");
    rt.block_on(async {
        let mut tx = write_tx(store);
        for i in 0..rows {
            let id = (i as u64).to_be_bytes();
            tx.tb_insert(
                criterion::black_box(table),
                criterion::black_box(&id),
                criterion::black_box(b"v"),
            )
            .expect("tb_insert");
        }
        tx.commit().expect("commit");
    });
}

fn tb_insert(c: &mut Criterion) {
    let rt = runtime();
    let store = rt.block_on(async { open_store() });
    let scope = Scope::default();
    let table = scope.table("rows");

    c.bench_function("tb_insert", |b| {
        b.iter_batched(
            || uuid::Uuid::new_v4().into_bytes(),
            |id| {
                rt.block_on(async {
                    let mut tx = write_tx(&store);
                    tx.tb_insert(
                        criterion::black_box(table),
                        criterion::black_box(&id),
                        criterion::black_box(b"v"),
                    )
                    .expect("tb_insert");
                    tx.commit().expect("commit");
                });
            },
            BatchSize::SmallInput,
        );
    });
}

fn tb_get(c: &mut Criterion) {
    let rt = runtime();
    let store = rt.block_on(async { open_store() });
    let scope = Scope::default();

    let mut group = c.benchmark_group("tb_get");
    for &rows in TB_READ_ROW_COUNTS {
        seed_table(&rt, &store, rows);
        let id = ((rows / 2) as u64).to_be_bytes();
        let table = scope.table("rows");

        group.bench_with_input(BenchmarkId::from_parameter(rows), &rows, |b, _| {
            b.iter(|| {
                let mut tx = common::read_tx(&store);
                let out = tx
                    .tb_get(criterion::black_box(table), criterion::black_box(&id))
                    .expect("tb_get");
                criterion::black_box(out);
            });
        });
    }
    group.finish();
}

fn tb_list(c: &mut Criterion) {
    let rt = runtime();
    let store = rt.block_on(async { open_store() });
    let scope = Scope::default();

    let mut group = c.benchmark_group("tb_list");
    for &rows in TB_READ_ROW_COUNTS {
        seed_table(&rt, &store, rows);
        let table = scope.table("rows");

        group.bench_with_input(BenchmarkId::from_parameter(rows), &rows, |b, _| {
            b.iter(|| {
                let mut tx = common::read_tx(&store);
                let out = tx
                    .tb_list(criterion::black_box(table), None, None, None)
                    .expect("tb_list");
                criterion::black_box(out);
            });
        });
    }
    group.finish();
}

// ---------- blobs ----------

const BLOB_SIZES: &[usize] = &[1 << 10, 64 << 10, 1 << 20]; // 1 KiB, 64 KiB, 1 MiB

fn blob_store(c: &mut Criterion) {
    let rt = runtime();
    let store = rt.block_on(async { open_store() });
    let scope = Scope::default();
    let mut rng = seeded_rng();

    let mut group = c.benchmark_group("blob_store");
    for &size in BLOB_SIZES {
        let blob = random_value(&mut rng, size);
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                rt.block_on(async {
                    let mut tx = write_tx(&store);
                    let id = tx
                        .store_blob(criterion::black_box(scope), criterion::black_box(&blob))
                        .expect("store_blob");
                    criterion::black_box(id);
                    tx.commit().expect("commit");
                });
            });
        });
    }
    group.finish();
}

fn blob_resolve(c: &mut Criterion) {
    let rt = runtime();
    let store = rt.block_on(async { open_store() });
    let scope = Scope::default();
    let mut rng = seeded_rng();

    let mut group = c.benchmark_group("blob_resolve");
    for &size in BLOB_SIZES {
        let blob = random_value(&mut rng, size);

        // Seed the blob once.
        let id = rt.block_on(async {
            let mut tx = write_tx(&store);
            let id = tx.store_blob(scope, &blob).expect("store_blob");
            tx.commit().expect("commit");
            id
        });

        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                let mut tx = common::read_tx(&store);
                let out = tx
                    .resolve_blob(criterion::black_box(id))
                    .expect("resolve_blob");
                criterion::black_box(out);
            });
        });
    }
    group.finish();
}

fn blob_link_and_resolve_path(c: &mut Criterion) {
    let rt = runtime();
    let store = rt.block_on(async { open_store() });
    let scope = Scope::default();
    let mut rng = seeded_rng();

    let mut group = c.benchmark_group("blob_link_and_resolve_path");
    for &size in BLOB_SIZES {
        let blob = random_value(&mut rng, size);

        rt.block_on(async {
            let mut tx = write_tx(&store);
            let id = tx.store_blob(scope, &blob).expect("store_blob");
            tx.link_blob(scope.path("hot.bin"), id).expect("link_blob");
            tx.commit().expect("commit");
        });

        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                let mut tx = common::read_tx(&store);
                let out = tx
                    .resolve_path(criterion::black_box(scope).path(criterion::black_box("hot.bin")))
                    .expect("resolve_path");
                criterion::black_box(out);
            });
        });
    }
    group.finish();
}

// ---------- timeseries ----------

use db::domain::FieldValue;

const TS_COUNTS: &[usize] = &[1_000, 100_000];

type TsSample = (
    Vec<(String, String)>,
    Vec<(String, db::domain::FieldValue)>,
    db::domain::Timestamp,
);

#[allow(clippy::cast_precision_loss)]
fn ts_sample(i: u64) -> TsSample {
    let tags = vec![(String::from("host"), format!("h{}", i % 8))];
    let fields = vec![(String::from("v"), FieldValue::F64(i as f64))];
    (tags, fields, i as db::domain::Timestamp)
}

fn ts_publish_one(c: &mut Criterion) {
    let rt = runtime();
    let store = rt.block_on(async { open_store() });
    let scope = Scope::default();
    let counter = AtomicU64::new(0);

    c.bench_function("ts_publish_one", |b| {
        b.iter_batched(
            || counter.fetch_add(1, Ordering::Relaxed),
            |i| {
                let (tags, fields, ts) = ts_sample(i);
                rt.block_on(async {
                    let mut tx = write_tx(&store);
                    tx.publish_measurement(
                        criterion::black_box(scope),
                        criterion::black_box("cpu"),
                        criterion::black_box(tags),
                        criterion::black_box(fields),
                        criterion::black_box(ts),
                    )
                    .expect("publish_measurement");
                    tx.commit().expect("commit");
                });
            },
            BatchSize::SmallInput,
        );
    });
}

fn ts_find_range(c: &mut Criterion) {
    let rt = runtime();
    let store = rt.block_on(async { open_store() });
    let scope = Scope::default();

    let mut group = c.benchmark_group("ts_find_range");
    for &n in TS_COUNTS {
        // Seed `n` samples in one tx.
        rt.block_on(async {
            let mut tx = write_tx(&store);
            for i in 0..n as u64 {
                let (tags, fields, ts) = ts_sample(i);
                tx.publish_measurement(scope, "cpu", tags, fields, ts)
                    .expect("publish_measurement");
            }
            tx.commit().expect("commit");
        });

        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                let mut tx = common::read_tx(&store);
                let out = tx
                    .find_measurement(
                        criterion::black_box(scope),
                        criterion::black_box("cpu"),
                        None,
                        None,
                        None,
                        None,
                    )
                    .expect("find_measurement");
                criterion::black_box(out);
            });
        });
    }
    group.finish();
}

criterion_group!(
    name = domain_benches;
    config = Criterion::default();
    targets = kv_put, kv_get, kv_delete, tb_insert, tb_get, tb_list,
              blob_store, blob_resolve, blob_link_and_resolve_path,
              ts_publish_one, ts_find_range
);
criterion_main!(domain_benches);
