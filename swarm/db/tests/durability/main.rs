//! Process-kill durability harness for the `db` crate.
//!
//! The same binary plays two roles:
//!   * **orchestrator** (default): runs scenarios — spawns node workers,
//!     brokers replication between them, kills them, validates recovery.
//!   * **worker** (`DB_DURABILITY_WORKER` set): hosts one db node.
//!
//! Manual/nightly only — a plain `cargo test -p db` skips it:
//!
//! ```text
//! DB_DURABILITY=1 cargo test -p db --test durability
//! DB_DURABILITY=1 cargo test -p db --test durability -- crash_loop
//! DB_DURABILITY=1 STRESS_SECS=300 SEED=42 cargo test -p db --test durability -- stress
//! ```
//!
//! On failure each scenario prints its seed, the oracle summary, the tail of
//! the replica route log, and keeps its data dirs + worker logs for forensics.

mod cluster;
mod oracle;
mod proto;
mod scenarios;
mod worker;

use std::future::Future;
use std::pin::Pin;
use std::process::ExitCode;

type ScenarioFn = fn(u64) -> Pin<Box<dyn Future<Output = anyhow::Result<()>>>>;

fn registry() -> Vec<(&'static str, ScenarioFn)> {
    vec![
        ("kill_during_write_burst", |seed| {
            Box::pin(scenarios::kill_during_write_burst(seed))
        }),
        ("kill_receiver_mid_catchup", |seed| {
            Box::pin(scenarios::kill_receiver_mid_catchup(seed))
        }),
        ("kill_sender_mid_catchup", |seed| {
            Box::pin(scenarios::kill_sender_mid_catchup(seed))
        }),
        ("crash_loop", |seed| Box::pin(scenarios::crash_loop(seed))),
        ("kill_during_gc", |seed| {
            Box::pin(scenarios::kill_during_gc(seed))
        }),
    ]
}

fn main() -> ExitCode {
    // Worker mode: this process hosts a single db node for the orchestrator.
    if std::env::var(proto::env::NAME).is_ok() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("unable to build worker runtime");
        return match rt.block_on(worker::run()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("worker failed: {err:#}");
                ExitCode::FAILURE
            }
        };
    }

    // Test runners discover tests by invoking every test binary with `--list`.
    // This custom harness exposes no libtest-style tests, so report an empty
    // list and exit; otherwise nextest tries to parse the skip message below as
    // a test entry and aborts the whole `db` run. Durability still runs via
    // `DB_DURABILITY=1 cargo test -p db --test durability`.
    if std::env::args().any(|arg| arg == "--list") {
        return ExitCode::SUCCESS;
    }

    if std::env::var("DB_DURABILITY").as_deref() != Ok("1") {
        println!(
            "durability harness skipped — set DB_DURABILITY=1 to run \
             (STRESS_SECS=n adds the randomized stress scenario)"
        );
        return ExitCode::SUCCESS;
    }

    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .collect();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("unable to build runtime");

    // Forensics: `… -- inspect <data-dir> <namespace>` prints a kept data
    // dir's sync points and keys without running any scenario.
    if args.first().map(String::as_str) == Some("inspect") {
        let dir = args.get(1).expect("inspect needs a data dir");
        let namespace = args.get(2).expect("inspect needs a namespace");
        // `Store::init` spawns background tasks, so it needs a runtime.
        let _guard = rt.enter();
        return match inspect(dir.into(), namespace) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("inspect failed: {err:#}");
                ExitCode::FAILURE
            }
        };
    }

    rt.block_on(run_scenarios(args))
}

async fn run_scenarios(filter: Vec<String>) -> ExitCode {
    let seed_override = std::env::var("SEED")
        .ok()
        .map(|s| s.parse::<u64>().expect("SEED must be a u64"));
    let stress_secs = std::env::var("STRESS_SECS")
        .ok()
        .map(|s| s.parse::<u64>().expect("STRESS_SECS must be a u64"));

    let selected = |name: &str| filter.is_empty() || filter.iter().any(|f| f == name);

    let mut failures = 0usize;
    let mut ran = 0usize;

    for (name, scenario) in registry() {
        if !selected(name) {
            continue;
        }
        ran += 1;
        // Fixed per-scenario default so plain runs are reproducible.
        let seed = seed_override.unwrap_or_else(|| fixed_seed(name));

        println!("=== {name} (seed {seed}) ===");
        let started = std::time::Instant::now();
        match scenario(seed).await {
            Ok(()) => println!("=== {name} passed in {:.1?} ===\n", started.elapsed()),
            Err(err) => {
                failures += 1;
                eprintln!("=== {name} FAILED in {:.1?} ===", started.elapsed());
                eprintln!("seed: {seed}");
                eprintln!("{err:#}\n");
            }
        }
    }

    // Stress runs when asked for by env or named explicitly.
    let stress_named = filter.iter().any(|f| f == "stress");
    if stress_secs.is_some() || stress_named {
        ran += 1;
        let secs = stress_secs.unwrap_or(60);
        let seed = seed_override.unwrap_or_else(rand::random);

        println!("=== stress for {secs}s (seed {seed}) ===");
        let started = std::time::Instant::now();
        match scenarios::stress(seed, secs).await {
            Ok(()) => println!("=== stress passed in {:.1?} ===\n", started.elapsed()),
            Err(err) => {
                failures += 1;
                eprintln!("=== stress FAILED in {:.1?} ===", started.elapsed());
                eprintln!("seed: {seed} (replay with SEED={seed} STRESS_SECS={secs})");
                eprintln!("{err:#}\n");
            }
        }
    }

    if ran == 0 {
        eprintln!("no scenario matched filter {filter:?}");
        return ExitCode::FAILURE;
    }

    if failures > 0 {
        eprintln!("{failures}/{ran} scenarios failed");
        ExitCode::FAILURE
    } else {
        println!("all {ran} scenarios passed");
        ExitCode::SUCCESS
    }
}

/// Print every sync point and every key of a kept node data dir.
fn inspect(dir: std::path::PathBuf, namespace: &str) -> anyhow::Result<()> {
    use db::domain;
    use db::store::{Store, TransactionOptions};

    let store: Store = Store::init(db::store::Options {
        directory: Some(dir),
        logic_clock: std::sync::Arc::new(uhlc::HLC::default()),
        gc_interval: Some(std::time::Duration::from_hours(1)),
    })?;

    let tx = store.begin_local(&TransactionOptions::read())?;
    let subject = domain::Subject::Namespace(namespace.to_string());
    let (lower, upper) = domain::SyncPoint::range_from_subject(&subject)?;

    println!("sync points:");
    let mut scopes = std::collections::BTreeSet::new();
    tx.collect_latest_heads(lower, upper, |scope, (epoch, ts, node), sm| {
        println!(
            "  {scope} ts={ts} epoch={epoch} node={} marker={:?} retention={:?}",
            uhlc::ID::try_from(node).map_or("?".into(), |id| id.to_string()),
            sm.marker,
            sm.retention_period,
        );
        scopes.insert((scope.namespace, scope.database, scope.schema));
        Ok(())
    })?;

    println!("keys:");
    let mut tx = store.begin_local(&TransactionOptions::read())?;
    for (namespace, database, schema) in &scopes {
        let scope = domain::Key::new_scope(namespace, database, schema);
        for key in tx.key_prefix(scope, "")? {
            let value = tx.key_get(scope.kv(&key))?;
            println!(
                "  {namespace}/{database}/{schema}:{key} = {:?}",
                value.map(|v| String::from_utf8_lossy(&v).into_owned()),
            );
        }
    }

    Ok(())
}

/// Stable, arbitrary per-scenario seed (FNV-1a over the name).
fn fixed_seed(name: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in name.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
