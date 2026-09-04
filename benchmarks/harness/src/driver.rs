//! Generic single-load-pass driver: everything about running a benchmark that isn't specific to
//! it. A specific benchmark only needs to implement [`BenchmarkScenario`]; this module owns
//! parsing the config file, waiting for completeness, collecting spans, and assembling the report
//! for one pass. A sweep across loads is one process per load — see `crate::config`'s module
//! docs — driven externally (e.g. `benchmarks/warehouse/run_sweep.sh`), not by this module.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bench_report::ratio_percent;
use cell_protocol::Sri;
use clap::Parser;
use test_framework::{
    SriAttribute,
    clients::db::spans_cover_hops,
    latency::{DurationCollection, Latency, LatencyCollection},
    metrics::CellInteractionMetricsSnapshot,
    scenario::SwarmTestCtx,
};
use uuid::Uuid;

const EVENT_BATCH_SPAN: &str = "cell_task::event_batch";
const EVENT_DISPATCH_SPAN: &str = "cell_task::event_dispatch";

use crate::config::BenchmarkConfig;
use crate::scenario::{BenchmarkScenario, DispatchedCalls};

#[derive(Parser)]
struct Args {
    /// path to this benchmark's TOML config file (see [`BenchmarkConfig`])
    #[clap(long)]
    config: PathBuf,

    /// this pass's load — one config file drives a whole sweep, one process per load, via e.g.
    /// `--load 100`, `--load 200`, ...
    #[clap(long)]
    load: u64,

    /// overrides the config file's `output_dir` — a relative path in the config resolves against
    /// this process's working directory, which a script driving a sweep from elsewhere on disk
    /// (e.g. `benchmarks/warehouse/run_sweep.sh`) generally shouldn't have to depend on getting
    /// right; passing an absolute path here sidesteps that.
    #[clap(long)]
    output_dir: Option<PathBuf>,
}

/// Runs `scenario` through one load pass (`--load`), printing a console summary and, if
/// `output_dir` is set, writing
/// [`bench_report::raw::MultiRunReport`] as `{output_dir}/{load}.json` — raw data only; use
/// `bench-report`'s `merge` binary to combine several loads' files (from separate runs of this
/// binary) into one report with a rendered PDF.
pub async fn run<Scenario: BenchmarkScenario>(scenario: Scenario) {
    let args = Args::parse();
    let mut config = BenchmarkConfig::<Scenario::Specialized>::load(&args.config);
    if let Some(output_dir) = args.output_dir {
        config.output_dir = Some(output_dir);
    }
    let load = args.load;

    let ctx = scenario.build_ctx(&config.specialized).await;
    let sri_names = scenario.sri_names(&config.specialized);
    let expected_hops = scenario.expected_hops(&config.specialized);
    let run_params = scenario.run_params(&config.specialized);

    println!("=== load {load}/sec ===");
    let (summary, load_detail) =
        run_pass(&ctx, &scenario, &config, &sri_names, &expected_hops, load).await;

    let report = bench_report::raw::MultiRunReport {
        title: scenario.title(),
        version: env!("BENCH_GIT_SHA").to_owned(),
        run: bench_report::raw::RunConfig::new(
            run_params
                .into_iter()
                .chain([("Timeout (seconds)".to_owned(), config.timeout.to_string())]),
        ),
        summary: bench_report::raw::LoadSummary::build(vec![load], &[summary], |sri| {
            resolve_sri(&sri_names, sri)
        }),
        detail: vec![load_detail],
    };

    if let Some(output_dir) = config.output_dir {
        std::fs::create_dir_all(&output_dir).unwrap_or_else(|err| {
            panic!(
                "failed to create output directory {}: {err}",
                output_dir.display()
            )
        });

        let json_path = output_dir.join(format!("{load}.json"));
        let json_bytes = serde_json::to_vec_pretty(&report).expect("report serializes to JSON");
        std::fs::write(&json_path, json_bytes).unwrap_or_else(|err| {
            panic!("failed to write report to {}: {err}", json_path.display())
        });
    }
}

/// Drives, measures, and reports on the (single) load pass this process runs.
#[allow(clippy::too_many_lines)] // one linear measure/run/measure pass, not meaningfully splittable
async fn run_pass<Scenario: BenchmarkScenario>(
    ctx: &SwarmTestCtx,
    scenario: &Scenario,
    config: &BenchmarkConfig<Scenario::Specialized>,
    sri_names: &HashMap<Sri, String>,
    expected_hops: &[Vec<Sri>],
    load: u64,
) -> (
    bench_report::raw::PassSummary,
    bench_report::raw::LoadDetail,
) {
    ctx.force_flush_telemetry().await;
    let pass_started_at = SystemTime::now();
    let metrics_before = ctx.cell_interaction_metrics().await;
    let replication_before = ctx.replication_metrics().await;

    // `pass_index` only ever distinguished passes sharing one long-lived swarm process; with one
    // process per load now, it's always 0.
    let dispatched = scenario
        .dispatch(ctx, &config.specialized, 0, load, config.timeout)
        .await;

    // Wait for cell processing to actually finish *before* asking whether each call's trace is
    // complete — a call whose last hop hasn't run yet can't have a span for it, so judging trace
    // completeness any earlier just measures how far along processing happened to be at an
    // arbitrary moment, not whether it actually completed.
    let (metrics_after, completeness) = ctx
        .wait_for_completeness(
            Duration::from_millis(config.completeness_poll_interval_ms),
            config.completeness_stable_rounds,
            Duration::from_secs(config.drain_timeout),
        )
        .await;
    if !completeness.is_complete() {
        println!("warning: {}", completeness.explanation());
    }

    let (event_batch, db_state, event_topics) = tokio::join!(
        event_batch_summary(ctx, pass_started_at),
        ctx.cell_db_state(),
        ctx.event_topic_state(),
    );
    print_db_backlog(&db_state, &event_topics, sri_names);

    let externally_dispatched = matches!(dispatched.calls, DispatchedCalls::Known(_));
    let calls = resolve_calls(ctx, dispatched.calls, pass_started_at, expected_hops).await;

    let ingested_messages = calls.len() as u64;
    let successful_traces = calls.iter().filter(|call| call.trace_successful).count() as u64;
    let ingestion_loss = dispatched
        .expected_messages
        .saturating_sub(ingested_messages);
    println!(
        "ingestion: {ingested_messages}/{} call(s) produced ({}) — {ingestion_loss} call(s) not produced",
        dispatched.expected_messages,
        percent(ingested_messages, dispatched.expected_messages)
    );
    println!(
        "trace completeness: {successful_traces}/{ingested_messages} call(s) saw all {} expected hop(s)",
        expected_hops.len()
    );

    // captured before `calls` is consumed below, so the JSON report (if requested) can carry the
    // exact per-call data the aggregate distribution below is computed from.
    let raw_calls: Vec<bench_report::raw::RawCall> = calls
        .iter()
        .map(|call| to_raw_call(call, sri_names))
        .collect();
    // Complete traces only. A call whose trace lost its spans gets `full = 0`
    // from `Latency::compute` (end falls back to t0), and a partial one is
    // measured over whichever prefix did arrive — so folding them in makes
    // trace loss look like *low* latency. A pass that got worse would then
    // report a better mean and p95 than one that got better, in the same number
    // the PDF and the console lead with. Loss has its own line above; this
    // number answers "how long did a call take when we could see all of it".
    let latency = calls
        .into_iter()
        .filter(|call| call.trace_successful)
        .map(|call| call.latency)
        .collect::<LatencyCollection>()
        .distribution();

    // a `Known` call was dispatched by us, from outside the swarm, so it counts as an externally
    // injected command here; a `Discovered` one was sent by a cell already inside the swarm
    // (e.g. a timer-driven producer), whose own `commands_sent` already accounts for it — adding
    // it again as "externally injected" would double-count it as loss that never happened.
    let metrics_delta = metrics_after.delta_since(&metrics_before);
    let externally_injected_commands = if externally_dispatched {
        ingested_messages
    } else {
        0
    };
    let internal_loss = metrics_delta.loss(externally_injected_commands, 0);
    if internal_loss.any() {
        println!(
            "internal loss: {} command(s), {} event(s) sent but never received inside the swarm",
            internal_loss.commands_lost, internal_loss.events_lost
        );
    }

    print_latency(&latency, sri_names);

    let hop_coverage = scenario.hop_coverage(&config.specialized, &metrics_delta);
    print_hop_coverage(&hop_coverage, ingested_messages);
    print_metrics(&metrics_delta, sri_names);
    print_replication(
        &ctx.replication_metrics()
            .await
            .delta_since(replication_before),
        metrics_delta.totals().commands_sent,
    );

    let (full_latency, cells) =
        bench_report::raw::split_latency(&latency, |sri| resolve_sri(sri_names, sri));

    let summary = bench_report::raw::PassSummary {
        completeness,
        ingestion: bench_report::raw::Ingestion {
            ingested: ingested_messages,
            expected: dispatched.expected_messages,
            loss: ingestion_loss,
            percent: ratio_percent(ingested_messages, dispatched.expected_messages),
        },
        trace_completeness: bench_report::raw::TraceCompleteness {
            successful_traces,
            ingested: ingested_messages,
            expected_hop_count: expected_hops.len(),
            percent: ratio_percent(successful_traces, ingested_messages),
        },
        full_latency,
        event_batch,
        hop_coverage,
        metrics: metrics_delta,
        db_backlog: db_state
            .iter()
            .map(|c| (c.sri, c.commands_remaining))
            .collect(),
        event_topics: event_topics
            .iter()
            .map(|t| (t.event.clone(), t.produced))
            .collect(),
    };
    let load_detail = bench_report::raw::LoadDetail {
        load,
        cells,
        raw_calls,
    };

    (summary, load_detail)
}

/// One dispatched call's outcome, as observed from outside the swarm.
struct CallRecord {
    /// trace id generated for this call, correlating it with its spans in the swarm's telemetry
    /// backend.
    trace_id: Uuid,
    /// when this call was dispatched, as nanoseconds since the Unix epoch.
    sent_at_unix_nanos: u64,
    /// whether this call's trace contained a span for every expected hop.
    trace_successful: bool,
    /// this call's latency breakdown, computed from whichever spans were found for its trace id.
    latency: Latency,
}

/// Resolves a pass's calls, dispatching to [`collect_calls`] or [`discover_calls`] depending on
/// how [`BenchmarkScenario::dispatch`] produced them — see [`DispatchedCalls`].
async fn resolve_calls(
    ctx: &SwarmTestCtx,
    calls: DispatchedCalls,
    pass_started_at: SystemTime,
    expected_hops: &[Vec<Sri>],
) -> Vec<CallRecord> {
    match calls {
        DispatchedCalls::Known(calls) => collect_calls(ctx, calls, expected_hops).await,
        DispatchedCalls::Discovered => discover_calls(ctx, pass_started_at, expected_hops).await,
    }
}

/// Queries spans for every dispatched call's trace id and builds a [`CallRecord`] per call,
/// computing its latency breakdown and whether its trace covered every expected hop. Call this
/// only after the swarm has finished processing ([`SwarmTestCtx::wait_for_completeness`]) —
/// `force_flush_telemetry` (which completeness-polling already calls every round) blocks until
/// the trace exporter has durably written every span created so far, so by the time completeness
/// is determined, every span that's ever going to exist for this pass already does; one query is
/// enough, no retry needed.
async fn collect_calls(
    ctx: &SwarmTestCtx,
    dispatched: Vec<(SystemTime, Uuid)>,
    expected_hops: &[Vec<Sri>],
) -> Vec<CallRecord> {
    // One bulk fetch grouped by trace id, instead of a fresh `await_span_hops` poll loop per call
    // — with hundreds of calls that per-call re-scan of the whole trace table made this path
    // minutes slower.
    let trace_ids: Vec<_> = dispatched
        .iter()
        .map(|(_sent_at, trace_id)| *trace_id)
        .collect();

    let spans_by_trace = ctx.query_spans_for_traces(&trace_ids).await;

    // Diagnostic only — helps tell "nothing was ever written/read for these traces" apart from
    // "spans exist but a specific hop's SRI attribute never showed up", when trace completeness
    // comes back unexpectedly low. `per_hop_coverage` counts *traces* that touched a hop at all
    // (a duplicated hop still counts once here), so it can't by itself explain a
    // processed-commands percentage above 100% — `per_hop_span_count` additionally counts every
    // span at that hop, so the gap between the two is exactly how much of that overage is the
    // same call's hop actually running (and emitting a span) more than once, as opposed to a
    // measurement artifact.
    let total_spans: usize = spans_by_trace.values().map(Vec::len).sum();
    let hop_span_counts = |hop: &[Sri]| -> (usize, usize) {
        spans_by_trace.values().fold((0, 0), |(traces, spans), s| {
            let hits = s
                .iter()
                .filter_map(SriAttribute::sri)
                .filter(|sri| hop.contains(sri))
                .count();
            (traces + usize::from(hits > 0), spans + hits)
        })
    };
    let per_hop: Vec<(usize, usize)> = expected_hops
        .iter()
        .map(|hop| hop_span_counts(hop))
        .collect();
    let per_hop_coverage: Vec<usize> = per_hop.iter().map(|(traces, _)| *traces).collect();
    println!(
        "trace query: {total_spans} span(s) found across {} trace id(s); traces with a span at hop [{}]",
        trace_ids.len(),
        per_hop_coverage
            .iter()
            .enumerate()
            .map(|(i, count)| format!("{i}: {count}/{}", trace_ids.len()))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let duplicated_hops: Vec<String> = per_hop
        .iter()
        .enumerate()
        .filter(|(_, (traces, spans))| spans > traces)
        .map(|(i, (traces, spans))| format!("{i}: {spans}/{traces}"))
        .collect();
    if !duplicated_hops.is_empty() {
        println!(
            "duplicate hop delivery: some traces saw a hop run more than once [{}] \
             (span count/traces touched) — the same call's work happened multiple times, not \
             just a reporting artifact",
            duplicated_hops.join(", ")
        );
    }

    dispatched
        .into_iter()
        .map(|(sent_at, trace_id)| {
            let spans = spans_by_trace.get(&trace_id).cloned().unwrap_or_default();
            let trace_successful = spans_cover_hops(&spans, expected_hops);
            CallRecord {
                trace_id,
                sent_at_unix_nanos: unix_nanos(sent_at),
                trace_successful,
                // measured from the external caller's point of view (including dispatch time
                // before the first span starts), not just the in-swarm portion.
                latency: Latency::new_with_t0(&spans, sent_at),
            }
        })
        .collect()
}

/// Finds calls generated by a producer running *inside* the swarm — no dispatched call list
/// exists to correlate by trace id, so instead this queries every span created since
/// `pass_started_at`, groups them by trace id, and keeps only the traces that actually touch the
/// pipeline's entry hop (`expected_hops[0]`) — spans from something else entirely (e.g. a
/// deploy-time operation) share the same table but aren't calls this pass produced.
///
/// Each kept trace's "sent at" is the earliest hop-tagged span's start time within it — the same
/// t0 [`Latency::new`] computes internally, since there was no external dispatch to time it from.
async fn discover_calls(
    ctx: &SwarmTestCtx,
    pass_started_at: SystemTime,
    expected_hops: &[Vec<Sri>],
) -> Vec<CallRecord> {
    let Some(entry_hop) = expected_hops.first() else {
        return Vec::new();
    };

    let since = unix_nanos(pass_started_at);
    let grouped = ctx.query_spans_grouped_since(since).await;

    grouped
        .into_iter()
        .filter_map(|(trace_id, spans)| {
            let touches_entry = spans
                .iter()
                .filter_map(SriAttribute::sri)
                .any(|sri| entry_hop.contains(&sri));
            if !touches_entry {
                return None;
            }

            let sent_at_unix_nanos = spans
                .iter()
                .filter(|span| span.sri().is_some())
                .map(|span| span.start_time_unix_nano)
                .min()
                .unwrap_or(since);

            Some(CallRecord {
                trace_id,
                sent_at_unix_nanos,
                trace_successful: spans_cover_hops(&spans, expected_hops),
                latency: Latency::new(&spans),
            })
        })
        .collect()
}

/// Builds a [`bench_report::raw::RawCall`] from a [`CallRecord`], resolving each hop's `Sri` to
/// its human-readable display name via `names` (falling back to the raw SRI if unknown).
fn to_raw_call(call: &CallRecord, names: &HashMap<Sri, String>) -> bench_report::raw::RawCall {
    bench_report::raw::RawCall {
        trace_id: call.trace_id,
        sent_at_unix_nanos: call.sent_at_unix_nanos,
        trace_successful: call.trace_successful,
        full_latency_nanos: nanos(call.latency.full),
        cells: call
            .latency
            .cells
            .iter()
            .map(|cell| bench_report::raw::RawCellHop {
                sri: resolve_sri(names, &cell.sri),
                offset_nanos: nanos(cell.offset),
                duration_nanos: nanos(cell.duration),
            })
            .collect(),
    }
}

/// Resolves `sri` to its human-readable display name via `names`, falling back to the raw SRI
/// (as a string) for any cell the scenario didn't name.
fn resolve_sri(names: &HashMap<Sri, String>, sri: &Sri) -> String {
    names.get(sri).cloned().unwrap_or_else(|| sri.to_string())
}

/// Builds a [`bench_report::raw::EventBatchSummary`] from every `cell_task::event_batch` span
/// (and its `cell_task::event_dispatch` children) started since `pass_started_at`. These spans
/// aren't tied to any call's trace — a batch can serve several different calls — so they're
/// queried by name across the whole run and time-windowed to this pass instead of looked up per
/// call the way [`collect_calls`] finds latency spans.
///
/// Span counts/durations a benchmark run realistically produces never lose precision converting
/// to `f64` (well under 2^52), so the casts below are exact in practice.
#[allow(clippy::cast_precision_loss)]
async fn event_batch_summary(
    ctx: &SwarmTestCtx,
    pass_started_at: SystemTime,
) -> bench_report::raw::EventBatchSummary {
    let since = unix_nanos(pass_started_at);

    let dispatches_in_pass = ctx
        .query_spans_by_name(EVENT_DISPATCH_SPAN)
        .await
        .iter()
        .filter(|span| span.start_time_unix_nano >= since)
        .count();

    let mut durations = DurationCollection::default();
    for span in ctx
        .query_spans_by_name(EVENT_BATCH_SPAN)
        .await
        .iter()
        .filter(|span| span.start_time_unix_nano >= since)
    {
        durations.push(
            span.end_time_unix_nano
                .saturating_sub(span.start_time_unix_nano) as f64,
        );
    }
    let batches = durations.len() as u64;

    let duration = if durations.is_empty() {
        bench_report::raw::DistributionJson {
            samples: 0,
            mean_nanos: 0,
            median_nanos: 0,
            std_deviation_nanos: 0,
            p95_nanos: 0,
            p99_nanos: 0,
        }
    } else {
        (&durations.distribution()).into()
    };

    bench_report::raw::EventBatchSummary {
        batches,
        duration,
        mean_batch_size: if batches == 0 {
            0.0
        } else {
            dispatches_in_pass as f64 / batches as f64
        },
    }
}

fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).expect("benchmark durations never exceed u64 nanoseconds")
}

fn unix_nanos(t: SystemTime) -> u64 {
    u64::try_from(
        t.duration_since(UNIX_EPOCH)
            .expect("call dispatch time must not be before the Unix epoch")
            .as_nanos(),
    )
    .expect("call dispatch time is too far in the future to fit in a u64 nanosecond count")
}

/// Prints the "EXTERNAL" latency block, resolving each cell's `Sri` to its human-readable name
/// (falling back to the raw SRI if unknown) instead of relying on `LatencyDistribution`'s
/// `Display` impl, which only knows about SRIs.
fn print_latency(
    latency: &test_framework::latency::LatencyDistribution,
    names: &HashMap<Sri, String>,
) {
    println!("EXTERNAL:");
    println!("full latency: {}", latency.full);
    for (sri, dist) in &latency.cells {
        println!("  {}:", resolve_sri(names, sri));
        print!("{dist}");
    }
}

/// Prints replication wire volumes for the pass.
///
/// `commands_sent` is the yardstick: every cell-to-cell send strands its append
/// on the sender's holder, so that many rows have to move by replication. Chunks
/// per pull against it says whether a stream pulls each commit as it lands or
/// sixty at a time, having left them waiting for an announce.
fn print_replication(repl: &test_framework::metrics::ReplicationMetrics, commands_sent: u64) {
    println!(
        "  replication: announces sent={} recv={}; changesets sent={} recv={}; \
         pulls={} mean={}us chunks={} ({} chunks/pull, {} rows to move)",
        repl.announces_sent,
        repl.announces_recv,
        repl.changesets_sent,
        repl.changesets_recv,
        repl.pulls,
        mean_micros(repl.pull_nanos, repl.pulls),
        repl.pull_chunks,
        repl.pull_chunks.checked_div(repl.pulls).unwrap_or(0),
        commands_sent,
    );
    println!(
        "  announce shape: scopes={} heads={} ({} heads/scope); baselines={}/{} scopes elided",
        repl.announce_scopes,
        repl.announce_heads,
        repl.announce_heads
            .checked_div(repl.announce_scopes)
            .unwrap_or(0),
        repl.announce_baselines,
        repl.announce_scopes,
    );
    println!(
        "  announce handling: {} handled; queued mean={}us, ran mean={}us",
        repl.announces_handled,
        mean_micros(repl.announce_queue_nanos, repl.announces_handled),
        mean_micros(repl.announce_nanos, repl.announces_handled),
    );
    println!(
        "  arrival age: {} rows pulled, mean age on arrival={}us over {} usable ({} clock-skewed)",
        repl.applied,
        mean_micros(
            repl.applied_age_nanos,
            repl.applied.saturating_sub(repl.applied_age_skewed),
        ),
        repl.applied.saturating_sub(repl.applied_age_skewed),
        repl.applied_age_skewed,
    );
    print_served("cells", repl.served_cells);
    print_served("other", repl.served_other);
    println!(
        "  pulls by role (cells): replica {} pulls/{} chunks; offload {} pulls/{} chunks",
        repl.pulls_replica_cells.pulls,
        repl.pulls_replica_cells.chunks,
        repl.pulls_offload_cells.pulls,
        repl.pulls_offload_cells.chunks,
    );
    println!(
        "  peeks served (cells): replica {} ({} rows); other {} ({} rows)",
        repl.peeks_replica_cells.peeks,
        repl.peeks_replica_cells.rows,
        repl.peeks_other_cells.peeks,
        repl.peeks_other_cells.rows,
    );
}

/// The serving node's own clock stamped the chunks it hands out (a cell's
/// transaction commits where it lands), so unlike the arrival age this wait is
/// measured single-clock: it is how long rows sat on the holder before a pull
/// took them.
fn print_served(label: &str, served: test_framework::metrics::ServedPulls) {
    println!(
        "  serve age ({label}): {} chunks over {} pulls, mean wait before pull={}us \
         over {} usable ({} clock-skewed); {} pages hit the size cap",
        served.chunks,
        served.pulls,
        mean_micros(
            served.age_nanos,
            served.chunks.saturating_sub(served.age_skewed),
        ),
        served.chunks.saturating_sub(served.age_skewed),
        served.age_skewed,
        served.pages_full,
    );
}

/// Mean of a nanosecond total over its count, in microseconds. 0 when nothing
/// was timed.
fn mean_micros(total_nanos: u64, count: u64) -> u64 {
    total_nanos.checked_div(count).unwrap_or(0) / 1_000
}

/// Prints the "METRICS" block, resolving each cell's `Sri` to its human-readable name (falling
/// back to the raw SRI if unknown) instead of relying on `CellInteractionMetricsSnapshot`'s
/// `Display` impl, which only knows about SRIs.
/// The mailbox depth the peek-serving node reported, sampled every 16th read,
/// meaned per tier. Read against the dispatched batch size: a mean far above
/// it means the head of the reader's window is jammed with rows it already
/// dispatched whose deletes have not landed where it reads; a mean near it
/// means the backlog simply is not on the node the peek resolved to.
fn print_depth(metrics: &CellInteractionMetricsSnapshot, names: &HashMap<Sri, String>) {
    let mut tiers: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for (sri, m) in &metrics.cells {
        let name = resolve_sri(names, sri);
        let tier = name
            .rsplit_once('.')
            .map_or(name.clone(), |(tier, _)| tier.to_owned());
        let entry = tiers.entry(tier).or_default();
        entry.0 += m.mailbox_depth_sum;
        entry.1 += m.mailbox_depth_samples;
    }

    let line = tiers
        .iter()
        .map(|(tier, (sum, samples))| {
            format!(
                "{tier} mean={} over {samples}",
                sum.checked_div(*samples).unwrap_or(0),
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    println!("  mailbox depth (sampled): {line}");
}

fn print_metrics(metrics: &CellInteractionMetricsSnapshot, names: &HashMap<Sri, String>) {
    let totals = metrics.totals();
    println!(
        "METRICS:\ntotal: commands rx={}, tx={}, failed={}; events rx={}, tx={}; \
         mailbox polls={} empty={} ({})",
        totals.commands_received,
        totals.commands_sent,
        totals.commands_failed,
        totals.events_received,
        totals.events_sent,
        totals.mailbox_polls,
        totals.mailbox_empty_polls,
        percent(totals.mailbox_empty_polls, totals.mailbox_polls)
    );
    let delivered = totals.commands_delivered_poke + totals.commands_delivered_backstop;
    println!(
        "  wakeups: notifications={} vs rx={} (lost {}); \
         delivered by poke={} backstop={} ({} on the backstop); \
         backstop polls={} empty={}",
        totals.mailbox_notifications,
        totals.commands_received,
        percent(
            totals
                .commands_received
                .saturating_sub(totals.mailbox_notifications),
            totals.commands_received,
        ),
        totals.commands_delivered_poke,
        totals.commands_delivered_backstop,
        percent(totals.commands_delivered_backstop, delivered),
        totals.mailbox_backstop_polls,
        totals.mailbox_backstop_empty_polls,
    );
    println!(
        "  round trips: commit mean={}us over {}; peek mean={}us over {}",
        mean_micros(totals.commit_nanos, totals.commits),
        totals.commits,
        mean_micros(totals.peek_nanos, totals.peeks),
        totals.peeks,
    );
    println!(
        "  parked: notified {} waits mean={}us; backstop {} waits mean={}us; read failures={}",
        totals.waits,
        mean_micros(totals.wait_nanos, totals.waits),
        totals.backstop_waits,
        mean_micros(totals.backstop_wait_nanos, totals.backstop_waits),
        totals.read_failures,
    );
    print_depth(metrics, names);
    let turn = totals.turn;
    println!(
        "  turn split: turn mean={}us over {}; recv lag mean={}us over {}; \
         dispatch mean={}us over {} (commit mean={}us; the rest is unaccounted)",
        mean_micros(turn.turn_nanos, turn.turns),
        turn.turns,
        mean_micros(turn.recv_lag_nanos, turn.recv_lags),
        turn.recv_lags,
        mean_micros(turn.dispatch_nanos, turn.dispatches),
        turn.dispatches,
        mean_micros(totals.commit_nanos, totals.commits),
    );
    println!(
        "  dispatch split: export lookup mean={}us over {}; guest call mean={}us over {} \
         (of which log host call mean={}us over {}); span mean={}us over {}",
        mean_micros(turn.export_lookup_nanos, turn.export_lookups),
        turn.export_lookups,
        mean_micros(turn.guest_call_nanos, turn.guest_calls),
        turn.guest_calls,
        mean_micros(turn.host_log_nanos, turn.host_logs),
        turn.host_logs,
        mean_micros(turn.span_nanos, turn.spans),
        turn.spans,
    );
    for (sri, m) in &metrics.cells {
        println!(
            "  {}: commands rx={}, tx={}, failed={}; events rx={}, tx={}",
            resolve_sri(names, sri),
            m.commands_received,
            m.commands_sent,
            m.commands_failed,
            m.events_received,
            m.events_sent
        );
    }
}

/// Prints ground-truth DB state (live `tb_count` reads, not exported/derived metrics): how many
/// commands are still sitting unprocessed in each cell's mailbox right now, and how many events
/// have ever been published under each event name (a permanent total, since events are never
/// deleted). Unlike [`print_metrics`]'s `commands_received`, a nonzero `commands_remaining` here
/// can't be an artifact of the mailbox cursor-visibility race — it's a row that's definitely
/// still there. Events aren't scoped per cell in the DB (every publisher of a name shares one
/// table), so they're reported per event name, not per cell — see
/// [`test_framework::clients::db::DbHandle::event_topic_state`].
fn print_db_backlog(
    cells: &[test_framework::clients::db::CellDbState],
    events: &[test_framework::clients::db::EventTopicState],
    names: &HashMap<Sri, String>,
) {
    let total_commands_remaining: usize = cells.iter().map(|c| c.commands_remaining).sum();
    println!("DB STATE:\ncommands remaining: total={total_commands_remaining}");
    for cell in cells {
        println!(
            "  {}: {}",
            resolve_sri(names, &cell.sri),
            cell.commands_remaining
        );
    }
    // A scenario that publishes no events at all (e.g. one whose hops are all commands) has
    // nothing for `event_topic_state` to discover a name for, so this is empty every pass, not
    // just this one — printing the header with nothing under it reads as a missing/broken
    // reading rather than "this scenario has no events".
    if !events.is_empty() {
        println!("events produced:");
        for event in events {
            println!("  {}: {}", event.event, event.produced);
        }
    }
}

fn print_hop_coverage(rows: &[(String, u64)], ingested_messages: u64) {
    println!("HOP COVERAGE:");
    for (label, processed) in rows {
        println!(
            "  {label}: {processed}/{ingested_messages} ({})",
            percent(*processed, ingested_messages)
        );
    }
}

fn percent(processed: u64, ingested_messages: u64) -> String {
    if ingested_messages == 0 {
        return "n/a".to_owned();
    }

    format!("{:.2}%", ratio_percent(processed, ingested_messages))
}
