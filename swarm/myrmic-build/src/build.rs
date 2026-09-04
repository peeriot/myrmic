//! Target-aware cell builds.
//!
//! Every target compiles the cell to a `wasm32-unknown-unknown` module via
//! [`compile_cell`]; embedded (esp) targets additionally AOT-compile that module
//! for the device. This is the entry point shared by consumers that need the
//! finished artifacts for a specific platform (the linux integration tests build
//! for [`Platform::Linux`]; the embedded tooling builds for `Esp32c{5,6,61}`).

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use myrmic_tags::Platform;

use crate::{cargo, compile_cell};

/// AOT artifacts (`.aot` + `.meta`) produced for an embedded target.
pub use aot_compiler::Artifacts as AotArtifacts;

/// Artifacts produced by [`build`] for a single cell and target.
pub struct CellBuild {
    /// The compiled `wasm32-unknown-unknown` module — produced for every target.
    pub wasm: PathBuf,
    /// AOT artifacts, present only for embedded targets.
    pub aot: Option<AotArtifacts>,
}

/// Which cargo target within a crate to compile to wasm.
///
/// A library is built as a `cdylib` (so it links to a standalone wasm module);
/// a binary is built as-is (it already links to a module).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CargoTarget {
    /// Pick automatically: the sole binary if there is exactly one, otherwise
    /// the sole library.
    Auto,
    /// The crate's library target.
    Lib,
    /// A named target, resolved against the crate's binaries (preferred) then
    /// its library.
    Named(String),
}

/// Builds the cell at `manifest_path` for `target`, compiling the cargo target
/// selected by `cargo_target`.
///
/// Produces the wasm module for every target; for embedded targets it also
/// AOT-compiles the module (via `wamrc`) into the crate's `target` directory.
pub fn build(
    manifest_path: &Path,
    target: Platform,
    cargo_target: &CargoTarget,
) -> anyhow::Result<CellBuild> {
    let wasm = compile_cell(manifest_path, cargo_target)?
        .into_iter()
        .find(|artifact| {
            artifact
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("wasm"))
        })
        .context("cell build produced no wasm artifact")?;

    let aot = match aot_target(target) {
        None => None,
        Some(aot_target) => {
            let out_dir = cargo::crate_info(manifest_path)
                .context("failed to resolve cargo target directory")?
                .target_directory;
            Some(aot_compiler::compile(&wasm, aot_target, &out_dir)?)
        }
    };

    Ok(CellBuild { wasm, aot })
}

/// Maps a build platform to its AOT-compiler counterpart, or `None` for
/// platforms that run the wasm directly (linux).
fn aot_target(target: Platform) -> Option<aot_compiler::Target> {
    match target {
        Platform::Linux => None,
        Platform::Esp32c5 => Some(aot_compiler::Target::ESP32C5),
        Platform::Esp32c6 => Some(aot_compiler::Target::ESP32C6),
        Platform::Esp32c61 => Some(aot_compiler::Target::ESP32C61),
    }
}
