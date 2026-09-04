//! Building the example cells this suite deploys, for both halves of the rig: AOT artifacts for
//! the device, plain wasm for the swarm's own Linux exec.
//!
//! Both go through the `myrmic-build` library rather than the myrmic CLI, so the HIL suite needs
//! no myrmic binary on the host.

use std::path::PathBuf;

use cell_protocol::ArtifactPlatform;
use test_framework::cell::{AotCellArtifact, CellArtifact};

/// Returns the AOT target tag used for embedded exec runtime discovery.
///
/// Controlled by the `EMBEDDED_TARGET` env var, which selects the chip; falls back to `ESP32C6`
/// for unrecognized values.
pub(super) fn aot_target() -> &'static str {
    match std::env::var("EMBEDDED_TARGET").as_deref() {
        Ok("ESP32C5") => "esp32c5",
        Ok("ESP32C61") => "esp32c61",
        _ => "esp32c6",
    }
}

/// Returns the `myrmic-build` target to compile cells for.
///
/// Controlled by the `EMBEDDED_TARGET` env var, which selects the chip; falls back to `ESP32C6`
/// for unrecognized values.
fn build_target() -> myrmic_build::Platform {
    match std::env::var("EMBEDDED_TARGET").as_deref() {
        Ok("ESP32C5") => myrmic_build::Platform::Esp32c5,
        Ok("ESP32C61") => myrmic_build::Platform::Esp32c61,
        _ => myrmic_build::Platform::Esp32c6,
    }
}

/// The rustc target triple the firmware is built for, derived from the ISA of the selected
/// `EMBEDDED_TARGET` (e.g. `riscv32imac` for the C5/C6/C61).
///
/// Because it reuses [`build_target`]/[`artifact_platform`], a future chip added to those helpers
/// (per PORTING.md §12) picks up the correct firmware path here with no further change.
pub(super) fn firmware_target_triple() -> String {
    format!(
        "{}-unknown-none-elf",
        artifact_platform(build_target()).as_str()
    )
}

/// Maps a build target to the `ArtifactPlatform` the class is registered under.
fn artifact_platform(target: myrmic_build::Platform) -> ArtifactPlatform {
    match target {
        myrmic_build::Platform::Esp32c5
        | myrmic_build::Platform::Esp32c6
        | myrmic_build::Platform::Esp32c61 => ArtifactPlatform::Riscv32imac,
        myrmic_build::Platform::Linux => unreachable!("embedded tests never build for linux"),
    }
}

/// `Cargo.toml` of the example cell in the directory `cell_name` under `tests/fixtures`.
fn manifest_path(cell_name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(cell_name)
        .join("Cargo.toml")
}

/// Builds the example cell in `cell_name` as an AOT artifact for the current embedded target.
///
/// The class is registered under `cell_name` with dashes folded to underscores, matching the
/// SDK's module naming.
pub(super) fn build_aot_cell(cell_name: &str) -> anyhow::Result<AotCellArtifact> {
    let target = build_target();
    let built = myrmic_build::build(
        &manifest_path(cell_name),
        target,
        &myrmic_build::CargoTarget::Auto,
    )
    .map_err(|e| anyhow::anyhow!("myrmic_build failed for {cell_name}: {e}"))?;
    let aot = built
        .aot
        .ok_or_else(|| anyhow::anyhow!("myrmic-build produced no AOT artifacts for {cell_name}"))?;
    Ok(AotCellArtifact {
        name: aot_class_name(cell_name),
        meta_path: aot.meta,
        aot_path: aot.aot,
        target: artifact_platform(target),
    })
}

/// Builds the example cell in `cell_name` as a plain wasm module for the swarm's Linux exec.
///
/// Registered under `<cell_name>.wasm`, the class name convention host-side wasm cells use.
pub(super) fn build_wasm_cell(cell_name: &str) -> anyhow::Result<CellArtifact> {
    let built = myrmic_build::build(
        &manifest_path(cell_name),
        myrmic_build::Platform::Linux,
        &myrmic_build::CargoTarget::Auto,
    )
    .map_err(|e| anyhow::anyhow!("myrmic_build failed for {cell_name}: {e}"))?;
    Ok(CellArtifact {
        name: format!("{cell_name}.wasm"),
        wasm_path: built.wasm,
    })
}

/// The class name an embedded cell is registered under.
pub(super) fn aot_class_name(cell_name: &str) -> String {
    cell_name.replace('-', "_")
}
