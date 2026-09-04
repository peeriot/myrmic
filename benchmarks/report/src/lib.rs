//! Shared benchmark run reporting: a JSON-serializable [`raw::MultiRunReport`] (one pass per
//! load value tested) plus a [`rendered::pdf`] renderer built on top of it.
//!
//! [`raw::PassSummary`] mirrors the summary numbers a benchmark prints to its console, and
//! [`raw::LoadDetail`] the full per-call data they're aggregated from, for a single load
//! pass. [`raw::MultiRunReport`] wraps a whole load sweep. A rendered report (currently PDF, via [`rendered::pdf::render`]) is
//! built entirely from a [`raw::MultiRunReport`] — either the one just produced by a run, or one
//! reloaded from a previously written JSON file — rather than recomputing anything from the
//! swarm directly. That keeps rendering a pure "raw data in, document out" step, decoupled from
//! how the raw data was collected.
//!
//! This crate is shared across benchmarks (not just `warehouse`): `raw` covers the parts of a
//! benchmark report that are common to any swarm benchmark (ingestion/latency/metrics), and the
//! vendored Typst packages + rendering machinery under `rendered` are entirely benchmark-agnostic.
//! A benchmark-specific schema (e.g. `warehouse`'s hop coverage tiers) and `.typ` template layout
//! still need to compose with these, typically by having the benchmark's own report module build
//! a [`raw::MultiRunReport`] and hand it to [`rendered::pdf::render`].

pub mod raw;
pub mod rendered;

/// Converts a [`std::time::Duration`] to nanoseconds, for report fields that store durations as
/// plain integers rather than relying on how `serde` happens to encode `Duration`.
fn nanos(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_nanos()).expect("benchmark durations never exceed u64 nanoseconds")
}

/// `numerator / denominator * 100`, or `0.0` if `denominator` is zero (kept total rather than
/// `NaN`/`Infinity`, neither of which `serde_json` can serialize).
///
/// Call counts a benchmark run realistically produces (well under 2^52) never lose precision
/// converting to `f64`, so the cast below is exact in practice.
#[allow(clippy::cast_precision_loss)]
pub fn ratio_percent(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        return 0.0;
    }
    (numerator as f64 / denominator as f64) * 100.0
}
