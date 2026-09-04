//! Semantic / SPARQL benches: update and query over a seeded graph.

use std::fmt::Write as _;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use db::domain::Scope;
use db::semantic::{Query, Update};
use db::store::fjall::Store;

mod common;

use common::{open_store, runtime, write_tx};

const TRIPLE_COUNTS: &[usize] = &[1_000, 10_000];

fn insert_data(n: usize) -> String {
    let mut s = String::from("PREFIX ex: <http://example.org/>\nINSERT DATA {\n");
    for i in 0..n {
        write!(s, "  ex:s{i} ex:p ex:o{i} .\n  ex:s{i} ex:t \"v{i}\" .\n").expect("write");
    }
    s.push_str("}\n");
    s
}

fn seed_graph(rt: &tokio::runtime::Runtime, store: &Store, n: usize) {
    let scope = Scope::default();
    let body = insert_data(n);
    let update = Update::parse(&body, None).expect("parse update");
    rt.block_on(async {
        let mut tx = write_tx(store);
        tx.sem_update(criterion::black_box(scope), criterion::black_box(update))
            .expect("sem_update");
        tx.commit().expect("commit");
    });
}

fn sem_update_insert(c: &mut Criterion) {
    let rt = runtime();
    let scope = Scope::default();

    let mut group = c.benchmark_group("sem_update_insert");
    for &n in TRIPLE_COUNTS {
        let body = insert_data(n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter_batched(
                || {
                    let store = rt.block_on(async { open_store() });
                    let update = Update::parse(&body, None).expect("parse update");
                    (store, update)
                },
                |(store, update)| {
                    rt.block_on(async {
                        let mut tx = write_tx(&store);
                        tx.sem_update(criterion::black_box(scope), criterion::black_box(update))
                            .expect("sem_update");
                        tx.commit().expect("commit");
                    });
                },
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}

fn sem_query_bgp(c: &mut Criterion) {
    let rt = runtime();
    let store = rt.block_on(async { open_store() });
    let scope = Scope::default();

    let q = "PREFIX ex: <http://example.org/>\n\
             SELECT ?s ?o WHERE { ?s ex:p ?o }";

    let mut group = c.benchmark_group("sem_query_bgp");
    for &n in TRIPLE_COUNTS {
        seed_graph(&rt, &store, n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                let query = Query::parse(q, None).expect("parse query");
                let mut tx = common::read_tx(&store);
                let out = tx
                    .sem_solution(
                        criterion::black_box(scope),
                        criterion::black_box(query),
                        criterion::black_box(0),
                        criterion::black_box(usize::MAX),
                    )
                    .expect("sem_solution");
                criterion::black_box(out);
            });
        });
    }
    group.finish();
}

fn sem_query_join_filter(c: &mut Criterion) {
    let rt = runtime();
    let store = rt.block_on(async { open_store() });
    let scope = Scope::default();

    let q = "PREFIX ex: <http://example.org/>\n\
             SELECT ?s ?v WHERE { ?s ex:p ?o . ?s ex:t ?v . FILTER(strlen(?v) > 0) }";

    let mut group = c.benchmark_group("sem_query_join_filter");
    for &n in TRIPLE_COUNTS {
        seed_graph(&rt, &store, n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                let query = Query::parse(q, None).expect("parse query");
                let mut tx = common::read_tx(&store);
                let out = tx
                    .sem_solution(
                        criterion::black_box(scope),
                        criterion::black_box(query),
                        criterion::black_box(0),
                        criterion::black_box(usize::MAX),
                    )
                    .expect("sem_solution");
                criterion::black_box(out);
            });
        });
    }
    group.finish();
}

fn sem_query_count(c: &mut Criterion) {
    let rt = runtime();
    let store = rt.block_on(async { open_store() });
    let scope = Scope::default();

    let q = "PREFIX ex: <http://example.org/>\n\
             SELECT (COUNT(*) AS ?c) WHERE { ?s ex:p ?o }";

    let mut group = c.benchmark_group("sem_query_count");
    for &n in TRIPLE_COUNTS {
        seed_graph(&rt, &store, n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                let query = Query::parse(q, None).expect("parse query");
                let mut tx = common::read_tx(&store);
                let out = tx
                    .sem_solution(
                        criterion::black_box(scope),
                        criterion::black_box(query),
                        criterion::black_box(0),
                        criterion::black_box(usize::MAX),
                    )
                    .expect("sem_solution");
                criterion::black_box(out);
            });
        });
    }
    group.finish();
}

criterion_group!(
    name = semantic_benches;
    config = Criterion::default();
    targets = sem_update_insert, sem_query_bgp, sem_query_join_filter, sem_query_count
);
criterion_main!(semantic_benches);
