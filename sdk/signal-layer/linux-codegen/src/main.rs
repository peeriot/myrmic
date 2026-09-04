//! `LinuxChipBackend` codegen CLI: generates a tokio pipeline crate from a
//! pipeline YAML + Linux manifest.
//!
//! Usage:
//!   linux-codegen --pipeline <yaml> --drivers <dir> --steps <dir> --out <dir>
//!   linux-codegen --pipeline <yaml> --manifest <yaml> --drivers <dir> --steps <dir> --out <dir>
//!
//! When `--manifest` is omitted, structural pipeline-YAML validation only
//! (SR-3a): validates the pipeline schema and exits 0 (valid) or 1 (invalid).
//! When `--manifest` is provided, full generation is performed (SR-3b).

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

use linux_codegen::{generate_linux, validate_pipeline_only};

#[derive(Parser)]
#[command(
    name = "linux-codegen",
    about = "Linux codegen — generates a standalone tokio pipeline crate \
             from a pipeline YAML + Linux device manifest"
)]
struct Args {
    /// Path to the pipeline YAML.
    #[arg(long)]
    pipeline: PathBuf,

    /// Path to the Linux device manifest YAML (optional).
    ///
    /// When omitted, performs structural pipeline-YAML validation only and
    /// exits without generating (SR-3a mode).
    #[arg(long)]
    manifest: Option<PathBuf>,

    /// Path to the sensor-drivers root directory.
    #[arg(long)]
    drivers: PathBuf,

    /// Path to the processing-steps root directory.
    #[arg(long)]
    steps: PathBuf,

    /// Output directory: the generated crate is written here as a standalone
    /// Cargo project (Cargo.toml + src/main.rs + `tests/tap_contract.rs`).
    ///
    /// **Path constraint**: the generated Cargo.toml uses relative `path = "../…"`
    /// dependencies that are resolved relative to `--out`.  These paths assume
    /// `--out` is located somewhere under `sdk/signal-layer/` in the
    /// swarm-sl-linux repo tree.  Placing `--out` elsewhere will cause `cargo
    /// build` of the generated crate to fail with "no such file or directory".
    #[arg(long)]
    out: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let pipeline_yaml = std::fs::read_to_string(&args.pipeline)
        .map_err(|e| anyhow::anyhow!("reading pipeline {}: {e}", args.pipeline.display()))?;

    // SR-3a: structural validation only when --manifest is omitted.
    let Some(manifest_path) = args.manifest else {
        match validate_pipeline_only(&pipeline_yaml) {
            Ok(pipeline) => {
                eprintln!(
                    "linux-codegen: pipeline `{}` is structurally valid",
                    pipeline.pipeline.id
                );
                return Ok(());
            }
            Err(e) => {
                eprintln!("linux-codegen: pipeline validation failed: {e}");
                std::process::exit(1);
            }
        }
    };

    // S2: warn if --out is not under sdk/signal-layer/ — the generated
    // Cargo.toml uses relative `path = "../…"` deps that only resolve from there.
    let out_abs = args.out.canonicalize().unwrap_or_else(|_| args.out.clone());
    let out_str = out_abs.to_string_lossy();
    if !out_str.contains("sdk/signal-layer") && !out_str.contains("sdk\\signal-layer") {
        eprintln!(
            "linux-codegen: WARNING: --out ({}) does not appear to be under \
             sdk/signal-layer/. The generated crate's Cargo.toml uses relative \
             path dependencies (e.g. path=\"../signal-layer-ipc\") that only resolve \
             when the output directory is inside sdk/signal-layer/. \
             `cargo build` of the generated crate may fail if run from another location.",
            args.out.display()
        );
    }

    // SR-3b: full generation with manifest.
    let crate_output = generate_linux(&manifest_path, &args.pipeline, &args.drivers, &args.steps)?;

    std::fs::create_dir_all(&args.out)?;
    crate_output.write_to(&args.out)?;

    eprintln!(
        "linux-codegen: generated pipeline crate `{}` in {}",
        crate_output.pipeline_id,
        args.out.display()
    );

    Ok(())
}
