//! The durability scenarios. Each takes a seed; the same seed replays the
//! same workload and kill timing decisions.

use crate::cluster::Cluster;
use crate::oracle::ValidateOpts;
use crate::proto::Op;
use anyhow::Context as _;
use db_commons::models::Scope;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::fmt::Write as _;
use std::time::Duration;

const NAMESPACE: &str = "dur";
const SETTLE: Duration = Duration::from_mins(2);

fn scopes() -> Vec<Scope> {
    vec![
        Scope::new(NAMESPACE, "db", "alpha"),
        Scope::new(NAMESPACE, "db", "beta"),
    ]
}

/// A conflict-heavy transaction: 1–4 ops over a shared key pool, at least one
/// put, distinct keys within the tx (so its per-key effect is unambiguous).
fn pooled_tx(rng: &mut StdRng, scopes: &[Scope], pool: usize) -> Vec<Op> {
    let n_ops = rng.random_range(1..=4usize);
    let mut keys = std::collections::HashSet::new();
    let mut ops = vec![];

    while ops.len() < n_ops {
        let key = format!("k{:03}", rng.random_range(0..pool));
        if !keys.insert(key.clone()) {
            continue;
        }
        let scope = scopes[rng.random_range(0..scopes.len())].clone();
        // The first op is always a put so a surviving tx is always provable.
        let del = !ops.is_empty() && rng.random_range(0..100) < 20;
        ops.push(if del {
            Op::Del { scope, key }
        } else {
            Op::Put {
                scope,
                key,
                payload: format!("p{}", rng.random_range(0..u64::MAX)),
            }
        });
    }

    ops
}

/// A conflict-free transaction over its own unique key — used where the goal
/// is volume (many sync points), not contention.
fn unique_tx(counter: &mut u64, rng: &mut StdRng, scopes: &[Scope]) -> Vec<Op> {
    *counter += 1;
    let scope = scopes[rng.random_range(0..scopes.len())].clone();
    vec![Op::Put {
        scope,
        key: format!("u{counter:05}"),
        payload: format!("p{}", rng.random_range(0..u64::MAX)),
    }]
}

/// Settle the cluster and run the full invariant sweep. Any failure keeps the
/// data dirs + worker logs and attaches the oracle and routing context.
async fn finish(cluster: &mut Cluster, opts: &ValidateOpts) -> anyhow::Result<()> {
    let scopes = scopes();
    let settled = cluster
        .settle(&scopes, opts.ignore_key_prefix, SETTLE)
        .await
        .context("cluster failed to settle");

    let failure = match settled {
        Err(err) => format!("{err:#}"),
        Ok(state) => {
            let violations = cluster.oracle.validate(&state, opts);
            if violations.is_empty() {
                println!("    {}", cluster.oracle.status_summary());
                return Ok(());
            }

            let mut report = format!("{} invariant violations:\n", violations.len());
            for violation in violations.iter().take(50) {
                report.push_str("  - ");
                report.push_str(violation);
                report.push('\n');
            }
            if violations.len() > 50 {
                let _ = writeln!(report, "  … and {} more", violations.len() - 50);
            }
            report
        }
    };

    let root = cluster.keep_artifacts();
    let mut report = failure;
    let _ = writeln!(report, "\noracle: {}", cluster.oracle.status_summary());
    report.push_str("last replica routes:\n");
    for entry in cluster.route_log_tail(30) {
        let _ = writeln!(report, "  {} -> {}  {}", entry.from, entry.to, entry.kind);
    }
    let _ = write!(
        report,
        "artifacts kept at {} (worker logs are <node>.log)",
        root.display()
    );
    anyhow::bail!(report)
}

/// Flood one node with concurrent transactions (waves keep the conflict rate
/// sane so most commits succeed), SIGKILL it mid-burst, restart it, and
/// verify nothing acked was lost and nothing partial became visible.
pub async fn kill_during_write_burst(seed: u64) -> anyhow::Result<()> {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut cluster = Cluster::new("burst", NAMESPACE, None)?;
    let scopes = scopes();

    cluster.spawn("a").await?;
    cluster.spawn("b").await?;

    // 10 waves of 30 concurrent txs; the kill lands inside wave 6 while its
    // transactions are mid-commit.
    for wave in 0..10 {
        for _ in 0..30 {
            let ops = pooled_tx(&mut rng, &scopes, 512);
            cluster.submit("a", ops, None)?;
        }

        if wave == 5 {
            cluster.kill("a").await?;
            cluster.spawn("a").await?;
        } else {
            cluster.pump_for(Duration::from_millis(30)).await?;
        }
    }

    // Everything sent to the live node must eventually resolve.
    let waited = std::time::Instant::now();
    while cluster.oracle.in_flight_count() > 0 {
        anyhow::ensure!(
            waited.elapsed() < Duration::from_mins(1),
            "{} txs never resolved",
            cluster.oracle.in_flight_count(),
        );
        cluster.pump_for(Duration::from_millis(50)).await?;
    }

    finish(&mut cluster, &ValidateOpts::default()).await
}

/// Build a long history on one node, then kill the peer while it is pulling
/// changesets, restart it, and verify it heals to full convergence.
pub async fn kill_receiver_mid_catchup(seed: u64) -> anyhow::Result<()> {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut cluster = Cluster::new("catchup-recv", NAMESPACE, None)?;
    let scopes = scopes();

    cluster.spawn("a").await?;

    let mut counter = 0;
    for _ in 0..600 {
        let ops = unique_tx(&mut counter, &mut rng, &scopes);
        cluster.submit("a", ops, None)?;
    }
    cluster.await_results(600, Duration::from_mins(3)).await?;

    // A fresh, empty peer joins and starts catching up.
    cluster.spawn("b").await?;
    cluster.announce_all()?;

    cluster
        .await_routed_to("b", "CHANGESET", 1, Duration::from_mins(1))
        .await?;
    // Random point inside the (seconds-long) ingestion stream.
    let nap = rng.random_range(100..1500u64);
    cluster.pump_for(Duration::from_millis(nap)).await?;

    cluster.kill("b").await?;
    cluster.spawn("b").await?;

    finish(&mut cluster, &ValidateOpts::default()).await
}

/// Same as above, but kill the node *serving* the catch-up mid-stream.
pub async fn kill_sender_mid_catchup(seed: u64) -> anyhow::Result<()> {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut cluster = Cluster::new("catchup-send", NAMESPACE, None)?;
    let scopes = scopes();

    cluster.spawn("a").await?;

    let mut counter = 0;
    for _ in 0..600 {
        let ops = unique_tx(&mut counter, &mut rng, &scopes);
        cluster.submit("a", ops, None)?;
    }
    cluster.await_results(600, Duration::from_mins(3)).await?;

    cluster.spawn("b").await?;
    cluster.announce_all()?;

    cluster
        .await_routed_to("b", "CHANGESET", 1, Duration::from_mins(1))
        .await?;
    let nap = rng.random_range(100..1500u64);
    cluster.pump_for(Duration::from_millis(nap)).await?;

    cluster.kill("a").await?;
    cluster.spawn("a").await?;

    finish(&mut cluster, &ValidateOpts::default()).await
}

/// Kill and restart the same node repeatedly under continuous writes —
/// recovery after recovery after recovery.
pub async fn crash_loop(seed: u64) -> anyhow::Result<()> {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut cluster = Cluster::new("crash-loop", NAMESPACE, None)?;
    let scopes = scopes();

    cluster.spawn("a").await?;
    cluster.spawn("b").await?;

    for _round in 0..5 {
        let resolved = cluster.oracle.resolved_count();
        for _ in 0..40 {
            let ops = pooled_tx(&mut rng, &scopes, 512);
            cluster.submit("a", ops, None)?;
        }
        // Let part of the batch resolve; the rest dies with the process.
        cluster
            .await_results(resolved + 20, Duration::from_mins(2))
            .await?;
        cluster.kill("a").await?;
        cluster.spawn("a").await?;
    }

    finish(&mut cluster, &ValidateOpts::default()).await
}

/// Aggressive GC + short-retention writes + a kill. Validation is restricted
/// to non-expiring data; expiring keys only need to not corrupt anything.
pub async fn kill_during_gc(seed: u64) -> anyhow::Result<()> {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut cluster = Cluster::new("gc", NAMESPACE, Some(150))?;
    let scopes = scopes();

    cluster.spawn("a").await?;
    cluster.spawn("b").await?;

    let mut counter = 0;
    for round in 0..6 {
        let resolved = cluster.oracle.resolved_count();
        let mut submitted = 0;

        for _ in 0..30 {
            let ops = pooled_tx(&mut rng, &scopes, 512);
            cluster.submit("a", ops, None)?;
            submitted += 1;
        }
        // Short-lived data that GC will erase out from under everyone.
        for _ in 0..10 {
            counter += 1;
            let scope = scopes[rng.random_range(0..scopes.len())].clone();
            let ops = vec![Op::Put {
                scope,
                key: format!("exp{counter:04}"),
                payload: "ephemeral".to_string(),
            }];
            cluster.submit("a", ops, Some(200))?;
            submitted += 1;
        }

        cluster
            .await_results(resolved + submitted, Duration::from_mins(1))
            .await?;
        // Let retention lapse and the 150ms GC actually run.
        cluster.pump_for(Duration::from_millis(400)).await?;

        if round == 3 {
            // A few in-flight txs ride into the kill alongside the GC churn.
            for _ in 0..15 {
                let ops = pooled_tx(&mut rng, &scopes, 512);
                cluster.submit("a", ops, None)?;
            }
            cluster.kill("a").await?;
            cluster.spawn("a").await?;
        }
    }

    finish(
        &mut cluster,
        &ValidateOpts {
            check_frontier: true,
            skip_retention: true,
            ignore_key_prefix: Some("exp"),
        },
    )
    .await
}

/// Randomized chaos for `secs` seconds: bursts, kills, restarts, announces.
/// At most one node is down at any moment.
pub async fn stress(seed: u64, secs: u64) -> anyhow::Result<()> {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut cluster = Cluster::new("stress", NAMESPACE, None)?;
    let scopes = scopes();

    for name in ["a", "b", "c"] {
        cluster.spawn(name).await?;
    }
    let names = ["a", "b", "c"];

    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    let mut last_spawn = std::time::Instant::now();

    while std::time::Instant::now() < deadline {
        let roll = rng.random_range(0..100);

        if roll < 55 {
            // A burst of conflicting txs at a random live node.
            let live = cluster.live_nodes();
            if let Some(name) = pick(&mut rng, &live) {
                for _ in 0..rng.random_range(1..=10) {
                    let ops = pooled_tx(&mut rng, &scopes, 256);
                    cluster.submit(&name, ops, None)?;
                }
            }
        } else if roll < 65 {
            // Kill someone — but never two at once, and give fresh nodes a
            // moment to boot before pulling the plug again.
            let all_up = names.iter().all(|n| cluster.is_live(n));
            if all_up && last_spawn.elapsed() > Duration::from_secs(2) {
                let name = names[rng.random_range(0..names.len())];
                cluster.kill(name).await?;
            }
        } else if roll < 75 {
            for name in names {
                if !cluster.is_live(name) {
                    cluster.spawn(name).await?;
                    last_spawn = std::time::Instant::now();
                }
            }
        } else if roll < 85 {
            cluster.announce_all()?;
        }

        cluster
            .pump_for(Duration::from_millis(rng.random_range(20..200)))
            .await?;
    }

    // Bring everyone back, let in-flight work resolve, then sweep.
    for name in names {
        if !cluster.is_live(name) {
            cluster.spawn(name).await?;
        }
    }
    let waited = std::time::Instant::now();
    while cluster.oracle.in_flight_count() > 0 {
        anyhow::ensure!(
            waited.elapsed() < Duration::from_mins(1),
            "{} txs never resolved on live nodes",
            cluster.oracle.in_flight_count(),
        );
        cluster.pump_for(Duration::from_millis(100)).await?;
    }

    finish(&mut cluster, &ValidateOpts::default()).await
}

fn pick(rng: &mut StdRng, items: &[String]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    Some(items[rng.random_range(0..items.len())].clone())
}
