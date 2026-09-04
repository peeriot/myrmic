//! The raw JSON report format for a benchmark load sweep.
//!
//! [`MultiRunReport`] mirrors the numbers printed to the console (ingestion/trace-completeness/
//! hop-coverage/metrics summaries), tabulated across every load value tested for side-by-side
//! comparison (see [`LoadSummary`]) — configuration that doesn't vary by load (e.g. fan-out) is
//! reported once, not repeated per load. Only the per-cell latency distributions and per-call
//! raw records are too granular to usefully tabulate across loads, so those stay broken out per
//! load in [`LoadDetail`]. This is the sole input a later rendered report (e.g. PDF) is expected
//! to build from.

use std::collections::BTreeMap;

use cell_protocol::Sri;
use serde::{Deserialize, Serialize};
use test_framework::{
    latency::{CellLatencyDistribution, DurationDistribution, LatencyDistribution},
    metrics::{CellInteractionMetrics, CellInteractionMetricsSnapshot},
    scenario::Completeness,
};
use uuid::Uuid;

use super::{nanos, ratio_percent};

/// Full raw JSON report for a load sweep (one or more passes at different load values) — or, as
/// of the per-load-process workflow, just one pass; see [`Self::merge`] for combining several of
/// those single-load reports back into a multi-load one.
#[derive(Serialize, Deserialize)]
pub struct MultiRunReport {
    /// title for this report, e.g. `"Warehouse Benchmark Report"` — shown as-is on a rendered
    /// report's title page/banner, so each benchmark controls its own wording.
    pub title: String,
    /// identifies the benchmark binary build that produced this report (currently a short git
    /// commit hash, baked in at compile time — see `benchmarks/harness/build.rs`). [`Self::merge`]
    /// refuses to combine reports with different values here: a report is only meaningful as a
    /// comparison across loads of the *same* code, so silently mixing builds would be misleading
    /// rather than merely inconvenient.
    pub version: String,
    /// configuration shared across every pass in the sweep (e.g. fan-out, fan-out strategy,
    /// timeout) — load itself varies per pass, so it isn't included here; see
    /// [`LoadSummary::loads`].
    pub run: RunConfig,
    /// ingestion/trace-completeness/latency/hop-coverage/metrics summaries, tabulated across
    /// every load value tested.
    pub summary: LoadSummary,
    /// per-load detail too granular to tabulate across loads: per-cell latency distributions and
    /// raw per-call records. One entry per load value, in the same order as
    /// [`LoadSummary::loads`].
    pub detail: Vec<LoadDetail>,
}

impl MultiRunReport {
    /// Merges reports that each cover a different load of the *same* run (e.g. one file per load,
    /// from spawning the benchmark binary once per load with a fresh swarm each time — see
    /// `benchmarks/warehouse/run_sweep.sh`) into one combined report, sorted by load ascending.
    ///
    /// A fresh swarm per load is exactly what makes each input trustworthy on its own: metrics
    /// from one load's run can't bleed into another's the way they could when a single long-lived
    /// swarm process ran every load's pass back to back (a backlog straggling past one pass's
    /// `drain_timeout` used to get silently counted against whichever pass was current when it
    /// finally caught up). Merging afterwards only concatenates already-independent results, it
    /// doesn't re-introduce that risk.
    ///
    /// # Panics
    ///
    /// Panics if `reports` is empty, or if any two reports disagree on `version`, `title`, or
    /// `run` (comparing different builds or benchmark configs side by side would produce a
    /// misleading report, not just a wrong-looking one) — or if their `hop_coverage`/`metrics`
    /// rows don't share the same labels/cells (expected, since they're meant to be the same
    /// benchmark scenario at different loads).
    #[must_use]
    pub fn merge(mut reports: Vec<Self>) -> Self {
        assert!(!reports.is_empty(), "need at least one report to merge");
        reports.sort_by_key(|report| report.summary.loads.first().copied().unwrap_or(0));

        for report in &reports[1..] {
            assert_eq!(
                report.version, reports[0].version,
                "refusing to merge reports built from different versions"
            );
            assert_eq!(
                report.title, reports[0].title,
                "refusing to merge reports with different titles"
            );
            assert_eq!(
                report.run, reports[0].run,
                "refusing to merge reports with different run configs"
            );
        }

        let mut reports = reports.into_iter();
        let first = reports.next().expect("checked non-empty above");
        let title = first.title;
        let version = first.version;
        let run = first.run;
        let mut summary = first.summary;
        let mut detail = first.detail;

        for report in reports {
            summary.merge_from(report.summary);
            detail.extend(report.detail);
        }

        Self {
            title,
            version,
            run,
            summary,
            detail,
        }
    }
}

/// Parameters a benchmark run was invoked with, as an ordered list of label/value pairs — kept
/// free-form (rather than fixed fields like `fan_out`) so any benchmark can report whichever
/// config is relevant to it, not just fields this crate knows about in advance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunConfig {
    /// one entry per reported parameter, in the order the benchmark listed them.
    pub params: Vec<RunParam>,
}

impl RunConfig {
    /// Builds a [`RunConfig`] from `(label, value)` pairs, in order.
    pub fn new(params: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>) -> Self {
        Self {
            params: params
                .into_iter()
                .map(|(label, value)| RunParam {
                    label: label.into(),
                    value: value.into(),
                })
                .collect(),
        }
    }
}

/// One run parameter, as a human-readable label and its value rendered as a string.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunParam {
    /// e.g. `"Fan-out"`, `"Timeout (seconds)"`.
    pub label: String,
    /// the parameter's value, already formatted as the benchmark wants it displayed (e.g. a
    /// `u64` rendered via `to_string`, or an enum variant's name).
    pub value: String,
}

/// Ingestion/trace-completeness/latency/hop-coverage/metrics summaries, tabulated across every
/// load value tested — every `Vec` field below (and every array nested inside
/// [`HopCoverageRow`]/[`MetricsRow`]) is indexed the same way as [`Self::loads`], so `foo[i]`
/// is always the value for `loads[i]`.
#[derive(Serialize, Deserialize)]
pub struct LoadSummary {
    /// the load values tested, in the order they were run.
    pub loads: Vec<u64>,
    /// whether — and if not, why not — that pass actually finished (reached the exact expected
    /// command/event counts) within its `drain_timeout`, per load value; see
    /// [`Completeness`]/`SwarmTestCtx::wait_for_completeness`. Treat a run with anything other
    /// than [`Completeness::Complete`] here with real suspicion rather than taking its counts at
    /// face value — [`Completeness::Stalled`] in particular means some of those counts are
    /// permanently short, not just still catching up.
    pub completeness: Vec<Completeness>,
    /// how many calls were actually dispatched vs. how many `load * timeout` implied should have
    /// been produced, per load value.
    pub ingestion: Vec<Ingestion>,
    /// trace-completeness summary at the call level, per load value.
    pub trace_completeness: Vec<TraceCompleteness>,
    /// end-to-end latency distribution, per load value.
    pub full_latency: Vec<DistributionJson>,
    /// `cell_task::event_batch` span summary (one span per event-listener poll iteration), per
    /// load value — see [`EventBatchSummary`].
    pub event_batch: Vec<EventBatchSummary>,
    /// named-hop command/event counts relative to that pass's `ingestion.ingested`, mirroring
    /// the console's "HOP COVERAGE" block — one row per hop label, each tabulated across loads.
    pub hop_coverage: Vec<HopCoverageRow>,
    /// cell interaction metrics (commands/events sent/received), mirroring the console's
    /// "METRICS" block — one row per cell (plus a `"total"` row), each tabulated across loads.
    pub metrics: Vec<MetricsRow>,
    /// ground-truth command backlog per cell at the end of each pass (live `tb_count` reads, not
    /// exported/derived metrics) — one row per cell, each tabulated across loads. See
    /// `SwarmTestCtx::cell_db_state`.
    pub db_backlog: Vec<DbBacklogRow>,
    /// ground-truth event count per event name/topic at the end of each pass (live `tb_count`
    /// reads) — one row per topic, each tabulated across loads. Events aren't scoped per cell in
    /// the DB (every publisher of a name shares one table), so this is topic-keyed rather than
    /// cell-keyed, unlike [`Self::db_backlog`]. See `SwarmTestCtx::event_topic_state`.
    pub event_topics: Vec<EventTopicRow>,
}

impl LoadSummary {
    /// Appends `other`'s per-load entries onto `self`'s, for [`MultiRunReport::merge`] — assumes
    /// `other` covers loads not already in `self`; the caller is responsible for that (and for
    /// sorting the result) since this only concatenates.
    ///
    /// # Panics
    ///
    /// Panics if `other`'s `hop_coverage`/`metrics` rows don't share `self`'s set of
    /// labels/cells — expected, since both are meant to be the same benchmark scenario.
    fn merge_from(&mut self, other: Self) {
        self.loads.extend(other.loads);
        self.completeness.extend(other.completeness);
        self.ingestion.extend(other.ingestion);
        self.trace_completeness.extend(other.trace_completeness);
        self.full_latency.extend(other.full_latency);
        self.event_batch.extend(other.event_batch);

        assert_eq!(
            self.hop_coverage.len(),
            other.hop_coverage.len(),
            "refusing to merge reports with different hop_coverage rows"
        );
        for row in other.hop_coverage {
            let existing = self
                .hop_coverage
                .iter_mut()
                .find(|existing| existing.label == row.label)
                .unwrap_or_else(|| panic!("no hop_coverage row for label {:?}", row.label));
            existing.counts.extend(row.counts);
            existing.percents.extend(row.percents);
        }

        assert_eq!(
            self.metrics.len(),
            other.metrics.len(),
            "refusing to merge reports with different metrics rows"
        );
        for row in other.metrics {
            let existing = self
                .metrics
                .iter_mut()
                .find(|existing| existing.cell == row.cell)
                .unwrap_or_else(|| panic!("no metrics row for cell {:?}", row.cell));
            existing.commands_received.extend(row.commands_received);
            existing.commands_sent.extend(row.commands_sent);
            existing.commands_failed.extend(row.commands_failed);
            existing.mailbox_polls.extend(row.mailbox_polls);
            existing.mailbox_empty_polls.extend(row.mailbox_empty_polls);
            existing
                .mailbox_backstop_polls
                .extend(row.mailbox_backstop_polls);
            existing
                .mailbox_backstop_empty_polls
                .extend(row.mailbox_backstop_empty_polls);
            existing
                .mailbox_notifications
                .extend(row.mailbox_notifications);
            existing
                .commands_delivered_poke
                .extend(row.commands_delivered_poke);
            existing
                .commands_delivered_backstop
                .extend(row.commands_delivered_backstop);
            existing.commit_nanos.extend(row.commit_nanos);
            existing.commits.extend(row.commits);
            existing.peek_nanos.extend(row.peek_nanos);
            existing.peeks.extend(row.peeks);
            existing.events_received.extend(row.events_received);
            existing.events_sent.extend(row.events_sent);
        }

        assert_eq!(
            self.db_backlog.len(),
            other.db_backlog.len(),
            "refusing to merge reports with different db_backlog rows"
        );
        for row in other.db_backlog {
            let existing = self
                .db_backlog
                .iter_mut()
                .find(|existing| existing.cell == row.cell)
                .unwrap_or_else(|| panic!("no db_backlog row for cell {:?}", row.cell));
            existing.commands_remaining.extend(row.commands_remaining);
        }

        assert_eq!(
            self.event_topics.len(),
            other.event_topics.len(),
            "refusing to merge reports with different event_topics rows"
        );
        for row in other.event_topics {
            let existing = self
                .event_topics
                .iter_mut()
                .find(|existing| existing.event == row.event)
                .unwrap_or_else(|| panic!("no event_topics row for event {:?}", row.event));
            existing.produced.extend(row.produced);
        }
    }
}

/// How many calls were actually dispatched vs. how many were expected, for one load value.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct Ingestion {
    /// number of calls actually dispatched by the load producer.
    pub ingested: u64,
    /// number of calls `load * timeout` implies should have been dispatched (`load` and
    /// `timeout` are both validated to be >= 1, so this is never zero).
    pub expected: u64,
    /// `expected - ingested`, saturating at zero: calls that should have been dispatched but
    /// weren't. Distinct from `trace_completeness.ingested - trace_completeness.successful_traces`,
    /// which counts calls that *were* dispatched but didn't fully arrive.
    pub loss: u64,
    /// `ingested / expected * 100`.
    pub percent: f64,
}

/// Trace-completeness summary at the call level: how many ingested calls had a span for every
/// expected hop in their chain, for one load value.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct TraceCompleteness {
    /// number of ingested calls whose trace contained a span for every expected hop.
    pub successful_traces: u64,
    /// number of ingested calls considered; equal to `ingestion.ingested` for the same load.
    pub ingested: u64,
    /// number of hops each call is expected to pass through (object -> zone -> central == 3).
    pub expected_hop_count: usize,
    /// `successful_traces / ingested * 100`.
    pub percent: f64,
}

/// A statistical distribution over a set of nanosecond duration samples.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct DistributionJson {
    /// number of samples the distribution was computed from.
    pub samples: usize,
    /// arithmetic mean, in nanoseconds.
    pub mean_nanos: u64,
    /// 50th percentile (median), in nanoseconds.
    pub median_nanos: u64,
    /// population standard deviation, in nanoseconds.
    pub std_deviation_nanos: u64,
    /// 95th percentile, in nanoseconds.
    pub p95_nanos: u64,
    /// 99th percentile, in nanoseconds.
    pub p99_nanos: u64,
}

/// Summary of the `cell_task::event_batch` span (one per event-listener poll iteration, covering
/// its DB poll and dispatching every event the poll returned) over one load pass. Not tied to any
/// call's trace — these spans are queried by name across the whole run and time-windowed to the
/// pass, not looked up per call — so this is a pass-wide aggregate, like `full_latency`, not a
/// per-cell breakdown like [`crate::raw::LoadDetail::cells`].
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct EventBatchSummary {
    /// number of poll iterations (batches) during this pass.
    pub batches: u64,
    /// distribution of each batch's duration (DB poll + dispatching every event it returned).
    pub duration: DistributionJson,
    /// average number of events landing in one batch (total `cell_task::event_dispatch` child
    /// spans divided by `batches`) — `0.0` if there were no batches.
    pub mean_batch_size: f64,
}

impl From<&DurationDistribution> for DistributionJson {
    fn from(d: &DurationDistribution) -> Self {
        Self {
            samples: d.samples,
            mean_nanos: nanos(d.mean),
            median_nanos: nanos(d.median),
            std_deviation_nanos: nanos(d.std_deviation),
            p95_nanos: nanos(d.p95),
            p99_nanos: nanos(d.p99),
        }
    }
}

/// One named hop's processed count relative to `ingestion.ingested`, tabulated across every load
/// value tested — `counts`/`percents` are indexed the same way as [`LoadSummary::loads`].
#[derive(Serialize, Deserialize)]
pub struct HopCoverageRow {
    /// e.g. `"Tier 1 object processed commands"`.
    pub label: String,
    /// number of commands/events processed, per load value.
    pub counts: Vec<u64>,
    /// `counts[i] / ingestion[i].ingested * 100`.
    pub percents: Vec<f64>,
}

/// Cell interaction metrics (commands/events sent/received) for one cell (or the aggregate
/// `"total"` across all cells), tabulated across every load value tested — every field besides
/// `cell` is indexed the same way as [`LoadSummary::loads`].
#[derive(Serialize, Deserialize)]
pub struct MetricsRow {
    /// the cell's resolved display name (or `"total"`), falling back to its raw SRI if the
    /// benchmark building this report doesn't know a name for it.
    pub cell: String,
    /// commands this cell received, per load value.
    pub commands_received: Vec<u64>,
    /// commands this cell sent on to another cell, per load value.
    pub commands_sent: Vec<u64>,
    /// handler invocations that failed and rolled back, so the command was
    /// redelivered — retry volume, per load value. Excluded from
    /// `commands_received`, which counts deliveries that actually completed.
    #[serde(default)]
    pub commands_failed: Vec<u64>,
    /// mailbox reads that followed a poke, and how many found nothing, per load
    /// value.
    #[serde(default)]
    pub mailbox_polls: Vec<u64>,
    #[serde(default)]
    pub mailbox_empty_polls: Vec<u64>,
    /// the same pair for reads driven by the 5s backstop instead.
    #[serde(default)]
    pub mailbox_backstop_polls: Vec<u64>,
    #[serde(default)]
    pub mailbox_backstop_empty_polls: Vec<u64>,
    /// table events delivered to this cell's mailbox watcher, counted before
    /// `Notify` coalesces them. Short of `commands_received` by however many
    /// pokes zenoh dropped in transit.
    #[serde(default)]
    pub mailbox_notifications: Vec<u64>,
    /// commands split by which signal got the reader moving — a poke, or the
    /// backstop tick that covers for a lost one.
    #[serde(default)]
    pub commands_delivered_poke: Vec<u64>,
    #[serde(default)]
    pub commands_delivered_backstop: Vec<u64>,
    /// wall nanoseconds spent in handler commit round trips, and how many —
    /// the mean is what a cell's throughput ceiling is made of.
    #[serde(default)]
    pub commit_nanos: Vec<u64>,
    #[serde(default)]
    pub commits: Vec<u64>,
    /// the same for the mailbox `tb_peek` round trip.
    #[serde(default)]
    pub peek_nanos: Vec<u64>,
    #[serde(default)]
    pub peeks: Vec<u64>,
    /// events this cell received, per load value.
    pub events_received: Vec<u64>,
    /// events this cell published, per load value.
    pub events_sent: Vec<u64>,
}

/// Ground-truth command backlog for one cell at the end of a pass — a live `tb_count` read, not
/// an exported/derived metric — tabulated across every load value tested. Unlike
/// [`MetricsRow::commands_received`], this can't be fooled by the mailbox cursor-visibility race
/// (see `SwarmTestCtx::command_backlog`): a nonzero value here is a row that's definitely still
/// in the table, whether or not any cursor has ever managed to see it.
#[derive(Serialize, Deserialize)]
pub struct DbBacklogRow {
    /// the cell's resolved display name (falling back to its raw SRI if unknown).
    pub cell: String,
    /// commands still sitting unprocessed in this cell's mailbox, per load value.
    pub commands_remaining: Vec<u64>,
}

/// Ground-truth event count for one event name/topic at the end of a pass — a live `tb_count`
/// read, not an exported/derived metric — tabulated across every load value tested. Events aren't
/// scoped per cell in the DB (every publisher of a name shares one table, see
/// `cell_protocol::scope_of_event`), so this is topic-keyed rather than cell-keyed, unlike
/// [`DbBacklogRow`]. A permanent total, not a snapshot — events are never deleted.
#[derive(Serialize, Deserialize)]
pub struct EventTopicRow {
    /// the event's name, as published (e.g. `"central_update"`).
    pub event: String,
    /// events ever published under this name, per load value.
    pub produced: Vec<u64>,
}

/// Per-load detail too granular to tabulate across loads: per-cell latency distributions and the
/// raw per-call records everything in [`LoadSummary`] is aggregated from.
#[derive(Serialize, Deserialize)]
pub struct LoadDetail {
    /// the load value this detail is for.
    pub load: u64,
    /// per-cell latency distributions, keyed by the cell's resolved display name (falling back
    /// to its raw SRI if unknown).
    pub cells: BTreeMap<String, CellLatencyJson>,
    /// one entry per ingested call, in the order calls were dispatched.
    pub raw_calls: Vec<RawCall>,
}

/// One cell's start/end/duration latency distributions.
#[derive(Serialize, Deserialize)]
pub struct CellLatencyJson {
    /// distribution of this cell's start time, offset from the call's t=0.
    pub start: DistributionJson,
    /// distribution of this cell's end time, offset from the call's t=0.
    pub end: DistributionJson,
    /// distribution of this cell's total handling time (`end - start`).
    pub duration: DistributionJson,
}

impl From<&CellLatencyDistribution> for CellLatencyJson {
    fn from(d: &CellLatencyDistribution) -> Self {
        Self {
            start: (&d.starts).into(),
            end: (&d.ends).into(),
            duration: (&d.durations).into(),
        }
    }
}

/// Everything [`LoadSummary::build`] needs from one load pass, before transposing across the
/// whole sweep.
pub struct PassSummary {
    pub completeness: Completeness,
    pub ingestion: Ingestion,
    pub trace_completeness: TraceCompleteness,
    pub full_latency: DistributionJson,
    pub event_batch: EventBatchSummary,
    /// (label, count) pairs, in the order the benchmark listed them; percent is computed
    /// against this pass's `ingestion.ingested`.
    pub hop_coverage: Vec<(String, u64)>,
    pub metrics: CellInteractionMetricsSnapshot,
    /// (sri, `commands_remaining`) pairs, one per deployed cell — see `SwarmTestCtx::cell_db_state`.
    pub db_backlog: Vec<(Sri, usize)>,
    /// (event name, produced) pairs, one per event topic — see
    /// `SwarmTestCtx::event_topic_state`.
    pub event_topics: Vec<(String, usize)>,
}

impl LoadSummary {
    /// Transposes one [`PassSummary`] per load value into a [`LoadSummary`] tabulated across the
    /// whole sweep, resolving each cell's `Sri` via `resolve` (e.g. its SRN).
    #[must_use]
    #[allow(clippy::too_many_lines)] // one transposition block per field, not meaningfully splittable
    pub fn build(
        loads: Vec<u64>,
        passes: &[PassSummary],
        resolve: impl Fn(&Sri) -> String,
    ) -> Self {
        assert_eq!(
            loads.len(),
            passes.len(),
            "one PassSummary is required per load value"
        );

        let mut hop_labels = Vec::new();
        for pass in passes {
            for (label, _) in &pass.hop_coverage {
                if !hop_labels.contains(label) {
                    hop_labels.push(label.clone());
                }
            }
        }
        let hop_coverage = hop_labels
            .into_iter()
            .map(|label| {
                let counts: Vec<u64> = passes
                    .iter()
                    .map(|pass| {
                        pass.hop_coverage
                            .iter()
                            .find(|(l, _)| *l == label)
                            .map_or(0, |(_, count)| *count)
                    })
                    .collect();
                let percents = counts
                    .iter()
                    .zip(passes)
                    .map(|(count, pass)| ratio_percent(*count, pass.ingestion.ingested))
                    .collect();
                HopCoverageRow {
                    label,
                    counts,
                    percents,
                }
            })
            .collect();

        let mut cell_names = std::collections::BTreeSet::new();
        for pass in passes {
            for sri in pass.metrics.cells.keys() {
                cell_names.insert(resolve(sri));
            }
            for (sri, _) in &pass.db_backlog {
                cell_names.insert(resolve(sri));
            }
        }
        let cell_metrics = |name: &str| -> Vec<CellInteractionMetrics> {
            passes
                .iter()
                .map(|pass| {
                    pass.metrics
                        .cells
                        .iter()
                        .find(|(sri, _)| resolve(sri) == name)
                        .map_or_else(CellInteractionMetrics::default, |(_, m)| *m)
                })
                .collect()
        };
        let metrics_row = |cell: &str, per_pass: Vec<CellInteractionMetrics>| MetricsRow {
            cell: cell.to_owned(),
            commands_received: per_pass.iter().map(|m| m.commands_received).collect(),
            commands_sent: per_pass.iter().map(|m| m.commands_sent).collect(),
            commands_failed: per_pass.iter().map(|m| m.commands_failed).collect(),
            mailbox_polls: per_pass.iter().map(|m| m.mailbox_polls).collect(),
            mailbox_empty_polls: per_pass.iter().map(|m| m.mailbox_empty_polls).collect(),
            mailbox_backstop_polls: per_pass.iter().map(|m| m.mailbox_backstop_polls).collect(),
            mailbox_backstop_empty_polls: per_pass
                .iter()
                .map(|m| m.mailbox_backstop_empty_polls)
                .collect(),
            mailbox_notifications: per_pass.iter().map(|m| m.mailbox_notifications).collect(),
            commands_delivered_poke: per_pass.iter().map(|m| m.commands_delivered_poke).collect(),
            commands_delivered_backstop: per_pass
                .iter()
                .map(|m| m.commands_delivered_backstop)
                .collect(),
            commit_nanos: per_pass.iter().map(|m| m.commit_nanos).collect(),
            commits: per_pass.iter().map(|m| m.commits).collect(),
            peek_nanos: per_pass.iter().map(|m| m.peek_nanos).collect(),
            peeks: per_pass.iter().map(|m| m.peeks).collect(),
            events_received: per_pass.iter().map(|m| m.events_received).collect(),
            events_sent: per_pass.iter().map(|m| m.events_sent).collect(),
        };
        let mut metrics = vec![metrics_row(
            "total",
            passes.iter().map(|pass| pass.metrics.totals()).collect(),
        )];
        metrics.extend(
            cell_names
                .iter()
                .map(|name| metrics_row(name, cell_metrics(name))),
        );

        let db_backlog = cell_names
            .iter()
            .map(|name| {
                let commands_remaining = passes
                    .iter()
                    .map(|pass| {
                        pass.db_backlog
                            .iter()
                            .find(|(sri, _)| &resolve(sri) == name)
                            .map_or(0, |(_, commands)| *commands as u64)
                    })
                    .collect();
                DbBacklogRow {
                    cell: name.clone(),
                    commands_remaining,
                }
            })
            .collect();

        let mut event_names = std::collections::BTreeSet::new();
        for pass in passes {
            for (event, _) in &pass.event_topics {
                event_names.insert(event.clone());
            }
        }
        let event_topics = event_names
            .into_iter()
            .map(|event| {
                let produced = passes
                    .iter()
                    .map(|pass| {
                        pass.event_topics
                            .iter()
                            .find(|(name, _)| name == &event)
                            .map_or(0, |(_, produced)| *produced as u64)
                    })
                    .collect();
                EventTopicRow { event, produced }
            })
            .collect();

        Self {
            loads,
            completeness: passes.iter().map(|p| p.completeness).collect(),
            ingestion: passes.iter().map(|p| p.ingestion).collect(),
            trace_completeness: passes.iter().map(|p| p.trace_completeness).collect(),
            full_latency: passes.iter().map(|p| p.full_latency).collect(),
            event_batch: passes.iter().map(|p| p.event_batch).collect(),
            hop_coverage,
            metrics,
            db_backlog,
            event_topics,
        }
    }
}

/// Resolves each cell's `Sri` in a [`LatencyDistribution`] to a display name via `resolve` (e.g.
/// its SRN) rather than the raw SRI, splitting it into the end-to-end distribution (goes into
/// [`PassSummary::full_latency`]) and the per-cell map (goes into [`LoadDetail::cells`]).
#[must_use]
pub fn split_latency(
    d: &LatencyDistribution,
    resolve: impl Fn(&Sri) -> String,
) -> (DistributionJson, BTreeMap<String, CellLatencyJson>) {
    let full = (&d.full).into();
    let cells = d
        .cells
        .iter()
        .map(|(sri, dist)| (resolve(sri), dist.into()))
        .collect();
    (full, cells)
}

/// One dispatched call's outcome: when it was sent, whether its trace covered every expected hop,
/// and the latency breakdown of every cell hop that was actually observed for it.
#[derive(Serialize, Deserialize)]
pub struct RawCall {
    /// trace id generated for this call, correlating it with its spans in the swarm's telemetry
    /// backend.
    pub trace_id: Uuid,
    /// when this call was dispatched, as nanoseconds since the Unix epoch.
    pub sent_at_unix_nanos: u64,
    /// whether this call's trace contained a span for every expected hop (object, zone, central)
    /// — i.e. whether this was a successful trace. If `false`, `cells` below has fewer than
    /// `trace_completeness.expected_hop_count` entries — whichever hop is missing is the one
    /// that didn't get its span recorded (or didn't arrive at all).
    pub trace_successful: bool,
    /// total elapsed time from dispatch to the last observed span's end, in nanoseconds.
    pub full_latency_nanos: u64,
    /// one entry per cell hop actually observed for this call, in the order spans were queried
    /// back (not necessarily the order the hops occurred in — sort by `offset_nanos` if needed).
    pub cells: Vec<RawCellHop>,
}

/// One cell's latency breakdown for a single call.
#[derive(Serialize, Deserialize)]
pub struct RawCellHop {
    /// the cell that produced this hop, as a resolved display name (falling back to its SRI if
    /// the benchmark building this report doesn't know a name for it).
    pub sri: String,
    /// this hop's start time, offset from the call's t=0, in nanoseconds.
    pub offset_nanos: u64,
    /// this hop's total handling time, in nanoseconds.
    pub duration_nanos: u64,
}
