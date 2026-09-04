//! Generic multi-load-sweep benchmark harness.
//!
//! A specific benchmark implements [`scenario::BenchmarkScenario`] (its topology, how to
//! dispatch one load pass, and its hop-coverage shape) and calls [`driver::run`] from `main`.
//! Everything else — the config file, looping over load values, waiting for completeness between
//! passes, collecting spans, and assembling the [`bench_report::raw::MultiRunReport`] — is
//! handled here, so it doesn't need reimplementing per benchmark.

pub mod config;
pub mod driver;
pub mod scenario;

pub use config::BenchmarkConfig;
pub use driver::run;
pub use scenario::BenchmarkScenario;
