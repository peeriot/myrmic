//! Generic, benchmark-agnostic configuration: one load pass plus whatever a specific benchmark
//! needs, opaque to everything here (see [`crate::scenario::BenchmarkScenario::Specialized`]).
//!
//! A sweep across several loads is driven by running this binary once per load — each with its
//! own fresh swarm process — rather than looping over a list of loads within one process; see
//! `benchmarks/warehouse/run_sweep.sh`. That's what makes each load's numbers trustworthy on
//! their own: with every load sharing one long-lived swarm, a backlog that missed one pass's
//! `drain_timeout` would keep draining into the *next* pass's measurement window, silently
//! crediting (or blaming) the wrong load for it.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// A benchmark run's configuration: settings shared across every load in a sweep, plus a
/// `specialized` section a specific benchmark defines and interprets itself. The load itself
/// isn't here — it's always given via `--load` (see `crate::driver::run`), since one config file
/// is meant to drive a whole sweep across processes and every pass needs a different value.
#[derive(Deserialize)]
pub struct BenchmarkConfig<S> {
    /// keep the pass's load for this amount of seconds.
    pub timeout: u64,

    /// how long to wait, per pass, for cell processing to actually finish (a backlog that's still
    /// draining, just slowly, gets this long to catch up) before giving up and reporting that
    /// pass's metrics/hop-coverage as they stand. Doesn't bound how long a *stalled* pass is
    /// waited on — see `completeness_stable_rounds`.
    #[serde(default = "default_drain_timeout")]
    pub drain_timeout: u64,

    /// how often, in milliseconds, to poll cell interaction metrics while waiting for
    /// completeness — kept coarse since each poll forces a telemetry flush, which has a real cost.
    #[serde(default = "default_completeness_poll_interval_ms")]
    pub completeness_poll_interval_ms: u64,

    /// consecutive unchanged polls required before declaring a pass's commands settled, or — once
    /// it's polling for the exact number of events expected — before giving up early on a pass
    /// whose progress has stalled short of that target rather than waiting out the rest of
    /// `drain_timeout`. A stall this long is never going to resume: it means a row has become
    /// permanently invisible to a mailbox's cursor (a known db-layer bug), not that a backlog is
    /// just slow to drain.
    #[serde(default = "default_completeness_stable_rounds")]
    pub completeness_stable_rounds: u32,

    /// write this pass's report to `{output_dir}/{load}.json`, in addition to the console
    /// summary — raw data only, no PDF; `bench-report`'s `merge` binary combines several loads'
    /// JSON files (from separate runs of this binary) into one report and renders that.
    #[serde(default)]
    pub output_dir: Option<PathBuf>,

    /// the benchmark-specific part of this config, meaningless to the generic driver.
    pub specialized: S,
}

fn default_drain_timeout() -> u64 {
    10
}

fn default_completeness_poll_interval_ms() -> u64 {
    500
}

fn default_completeness_stable_rounds() -> u32 {
    2
}

impl<S: serde::de::DeserializeOwned> BenchmarkConfig<S> {
    /// Reads and parses a TOML config file at `path`.
    ///
    /// # Panics
    ///
    /// Panics (with the path and underlying error) if the file can't be read or doesn't parse
    /// as a valid config — a benchmark's config is a startup precondition, not a recoverable
    /// runtime error.
    #[must_use]
    pub fn load(path: &Path) -> Self {
        let contents = std::fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("failed to read config {}: {err}", path.display()));
        toml::from_str(&contents)
            .unwrap_or_else(|err| panic!("failed to parse config {}: {err}", path.display()))
    }
}
