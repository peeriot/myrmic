//! Shared cell-build logic used by `myrmic-cli` and the integration-test
//! infrastructure.
//!
//! This crate holds the low-level primitives for compiling a cell logic crate to
//! wasm (and reading its `[package.metadata.myrmic]` build configuration). The
//! higher-level orchestration (app specs, bridges, archives) lives in the CLI.

pub use build::{AotArtifacts, CargoTarget, CellBuild, build};
pub(crate) use compile::compile_cell;
pub use myrmic_tags::Platform;

mod build;
pub mod cargo;
mod compile;
pub mod spawn_patch;
