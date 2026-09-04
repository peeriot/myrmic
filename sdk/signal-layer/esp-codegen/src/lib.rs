//! Library entry point for esp-codegen — exposes the generation logic so
//! integration tests can invoke it without going through the CLI.

use std::path::Path;

use anyhow::{Context, Result};
use indexmap::IndexMap;

use pipeline_codegen::ChipBackend;
use pipeline_codegen::descriptor::{DriverSchema, load_schema_from_yaml};
use pipeline_codegen::manifest::{BoardManifest, parse_manifest};
use pipeline_codegen::pipeline::PipelineFile;

mod backend;

pub use backend::Esp32Backend;

/// Run the full ESP32 generation pipeline from paths on disk.
///
/// Reads `board_yaml_path`, `pipeline_yaml_path`, resolves driver/step
/// descriptors from `drivers_root` / `steps_root`, validates, generates, and
/// returns the formatted Rust source as a `String`.
///
/// This is the same logic that `main()` runs; `main.rs` is a thin wrapper that
/// parses CLI args and delegates here.
pub fn generate_esp32(
    board_yaml_path: &Path,
    pipeline_yaml_path: &Path,
    drivers_root: &Path,
    steps_root: &Path,
) -> Result<String> {
    let board_yaml = std::fs::read_to_string(board_yaml_path)
        .with_context(|| format!("reading board manifest: {}", board_yaml_path.display()))?;
    let manifest = parse_manifest(&board_yaml)
        .with_context(|| format!("parsing board manifest: {}", board_yaml_path.display()))?;

    let pipeline_yaml = std::fs::read_to_string(pipeline_yaml_path)
        .with_context(|| format!("reading pipeline: {}", pipeline_yaml_path.display()))?;
    let pipeline: PipelineFile = serde_yaml::from_str(&pipeline_yaml)
        .with_context(|| format!("parsing pipeline: {}", pipeline_yaml_path.display()))?;

    let driver_schemas = load_schemas_for_drivers(drivers_root, &manifest, &pipeline)?;
    let step_schemas = load_schemas_for_steps(steps_root, &pipeline)?;

    let backend = Esp32Backend;

    let mut errors = pipeline_codegen::manifest::validate_manifest(&manifest);
    errors.extend(backend.validate_manifest(&manifest));
    errors.extend(
        pipeline_codegen::validate::validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &driver_schemas,
            &step_schemas,
            backend.pointer_width(),
        ),
    );
    if !errors.is_empty() {
        let joined = errors
            .iter()
            .map(|e| format!("  - {e}"))
            .collect::<Vec<_>>()
            .join("\n");
        anyhow::bail!("validation failed:\n{joined}");
    }

    pipeline_codegen::generate(
        &manifest,
        &pipeline,
        &driver_schemas,
        &step_schemas,
        &backend,
    )
    .context("code generation failed")
}

pub(crate) fn load_schemas_for_drivers(
    drivers_root: &Path,
    manifest: &BoardManifest,
    pipeline: &PipelineFile,
) -> Result<IndexMap<String, DriverSchema>> {
    let mut schemas = IndexMap::new();
    let device_ids = pipeline
        .sources
        .iter()
        .map(|s| s.device.as_str())
        .chain(pipeline.outlets.iter().map(|o| o.device.as_str()));
    for device_id in device_ids {
        let device = manifest
            .devices
            .iter()
            .find(|d| d.id == device_id)
            .with_context(|| format!("device `{device_id}` not found in manifest"))?;
        let driver_id = &device.driver;
        if schemas.contains_key(driver_id.as_str()) {
            continue;
        }
        let desc_path = drivers_root.join(driver_id).join("descriptor.yaml");
        let yaml = std::fs::read_to_string(&desc_path)
            .with_context(|| format!("reading driver descriptor: {}", desc_path.display()))?;
        let schema = load_schema_from_yaml(&yaml)
            .with_context(|| format!("parsing driver descriptor: {}", desc_path.display()))?;
        schemas.insert(driver_id.clone(), schema);
    }
    Ok(schemas)
}

pub(crate) fn load_schemas_for_steps(
    steps_root: &Path,
    pipeline: &PipelineFile,
) -> Result<IndexMap<String, DriverSchema>> {
    let mut schemas = IndexMap::new();
    for step in &pipeline.steps {
        let op = &step.op;
        if schemas.contains_key(op.as_str()) {
            continue;
        }
        let desc_path = steps_root.join(op).join("descriptor.yaml");
        if !desc_path.exists() {
            continue;
        }
        let yaml = std::fs::read_to_string(&desc_path)
            .with_context(|| format!("reading step descriptor: {}", desc_path.display()))?;
        let schema = load_schema_from_yaml(&yaml)
            .with_context(|| format!("parsing step descriptor: {}", desc_path.display()))?;
        schemas.insert(op.clone(), schema);
    }
    Ok(schemas)
}
