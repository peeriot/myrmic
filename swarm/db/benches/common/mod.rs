#![allow(dead_code)] // not all benches use every helper

use std::sync::Arc;

use db::store::Options;
use db::store::{Store, Transaction, TransactionOptions};
use rand::SeedableRng;
use rand::rngs::StdRng;
use tokio::runtime::{Builder, Runtime};

/// Fixed seed so bench inputs are reproducible across runs.
pub const BENCH_SEED: u64 = 0xD8_DB_BE_AC_BE_AC_BE_AC;

/// Mirrors `db::store::Options::test()` (which is `#[cfg(test)]`-only) so we
/// don't have to widen the crate's visibility.
pub fn bench_options() -> Options {
    let clock = uhlc::HLCBuilder::new().with_clock(uhlc::zero_clock).build();
    // Push the clock off the 0 sentinel.
    let _ = clock.new_timestamp();
    Options {
        directory: None,
        logic_clock: Arc::new(clock),
        gc_interval: None,
    }
}

pub fn open_store() -> Store {
    Store::init(bench_options()).expect("unable to open storage")
}

pub fn write_tx(store: &Store) -> Transaction {
    store
        .begin_local(&TransactionOptions::write())
        .expect("unable to start write tx")
}

pub fn read_tx(store: &Store) -> Transaction {
    store
        .begin_local(&TransactionOptions::read())
        .expect("unable to start read tx")
}

pub fn seeded_rng() -> StdRng {
    StdRng::seed_from_u64(BENCH_SEED)
}

/// One current-thread runtime per bench process — matches the test crate's
/// `#[tokio::test(flavor = "current_thread")]` convention.
pub fn runtime() -> Runtime {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("unable to build tokio runtime")
}

/// Build a buffer of `len` deterministic bytes from `rng`.
pub fn random_value(rng: &mut StdRng, len: usize) -> Vec<u8> {
    use rand::RngCore;
    let mut v = vec![0u8; len];
    rng.fill_bytes(&mut v);
    v
}
