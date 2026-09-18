//! Building a standalone Linux signal-layer pipeline crate.
//!
//! A Linux pipeline crate names itself in `[package.metadata.myrmic] pipeline =
//! "linux"`, which is what tells it apart from a cell crate. Its own `build.rs`
//! turns `board.yml` + `pipeline.yml` into the pipeline at compile time, so a
//! plain host `cargo build` produces the runnable binary; this just runs that
//! build for `myrmic build`, the way a firmware crate is built for its chip.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context as _;
use serde::Deserialize;

use crate::CargoTarget;
use crate::cargo;
use crate::compile::{Selector, resolve_selector};

/// The `[package.metadata.myrmic]` keys a Linux pipeline crate sets.
#[derive(Deserialize, Default)]
struct Metadata {
    pipeline: Option<PipelineKind>,
}

/// The signal-layer pipeline platform named by `[package.metadata.myrmic]
/// pipeline`. Only Linux builds as a standalone crate; a firmware pipeline is a
/// firmware crate instead (see [`crate::firmware`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PipelineKind {
    Linux,
}

/// Whether the crate at `manifest_path` is a standalone Linux signal-layer
/// pipeline, per `[package.metadata.myrmic] pipeline = "linux"`.
pub fn is_linux_pipeline(manifest_path: &Path) -> anyhow::Result<bool> {
    Ok(
        cargo::read_package_metadata::<Metadata>(manifest_path)?.and_then(|m| m.pipeline)
            == Some(PipelineKind::Linux),
    )
}

/// What a Linux pipeline build produced.
pub struct LinuxPipelineBuild {
    /// The linked native binary, ready to run.
    pub binary: PathBuf,
}

/// Builds the Linux pipeline crate at `manifest_path` in release mode for the
/// host, compiling the binary `cargo_target` selects. The crate's own
/// `build.rs` runs the signal-layer codegen during the compile, so nothing is
/// generated here first.
pub fn build(
    manifest_path: &Path,
    cargo_target: &CargoTarget,
) -> anyhow::Result<LinuxPipelineBuild> {
    let manifest_dir = manifest_path.parent().with_context(|| {
        format!(
            "manifest has no parent directory: {}",
            manifest_path.display()
        )
    })?;

    let bin = match resolve_selector(manifest_path, cargo_target)? {
        Selector::Bin(name) => name,
        Selector::Lib => {
            anyhow::bail!("a signal-layer pipeline is a binary; `lib` is not a pipeline target")
        }
    };

    let mut cmd = Command::new("cargo");
    cmd.current_dir(manifest_dir);
    cmd.args(["build", "--release", "--manifest-path"])
        .arg(manifest_path)
        .args(["--bin", &bin]);

    let mut binary = None;
    cargo::process_cargo_build(cmd, |artifact| {
        if artifact.executable {
            binary = Some(artifact.path.clone());
        }
    })?;
    let binary = binary.context("pipeline build produced no executable")?;

    Ok(LinuxPipelineBuild { binary })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_with(metadata: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Cargo.toml");
        std::fs::write(
            &path,
            format!("[package]\nname = \"pipe\"\nversion = \"0.1.0\"\n{metadata}"),
        )
        .unwrap();
        (dir, path)
    }

    #[test]
    fn is_linux_pipeline_reads_the_marker() {
        let (_dir, path) = manifest_with("[package.metadata.myrmic]\npipeline = \"linux\"\n");
        assert!(is_linux_pipeline(&path).unwrap());
    }

    #[test]
    fn is_linux_pipeline_is_false_without_the_marker() {
        let (_dir, path) = manifest_with("[package.metadata.myrmic]\nfirmware = \"esp32c6\"\n");
        assert!(!is_linux_pipeline(&path).unwrap());
        let (_dir, path) = manifest_with("");
        assert!(!is_linux_pipeline(&path).unwrap());
    }
}
