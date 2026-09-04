//! Rendered (human-facing document) reports, built from a [`super::raw::MultiRunReport`].
//!
//! Each renderer here takes only a [`super::raw::MultiRunReport`] as input — never the swarm or
//! telemetry backend directly — so rendering stays a pure "raw data in, document out" step,
//! decoupled from how the raw data was collected. That also means a previously saved raw JSON
//! report (from `--output`) can be re-rendered later without re-running the benchmark.

pub mod pdf;
