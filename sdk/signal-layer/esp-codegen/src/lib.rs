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

use include_dir::{Dir, include_dir};

/// Driver descriptors embedded at build time, so a firmware crate that depends
/// on this generator through its build script needs no `signal-modules` checkout
/// on disk. Only `descriptor.yaml` files are used; the rest of each crate rides
/// along in the host-side build tool and is never written out.
static EMBEDDED_DRIVERS: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../../../signal-modules/drivers");
/// Step descriptors embedded at build time. See [`EMBEDDED_DRIVERS`].
static EMBEDDED_STEPS: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../../../signal-modules/steps");

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

/// Same as [`generate_esp32`], but sourcing the driver/step descriptors from the
/// copies embedded in this crate instead of a `signal-modules` directory on disk.
///
/// The embedded descriptors are extracted to a temporary directory for the
/// duration of the call. When `custom_descriptors` is `Some`, it names a
/// directory laid out like `signal-modules` (with `drivers/` and/or `steps/`
/// subdirectories); its `descriptor.yaml` files are overlaid on top of the
/// embedded set, so a firmware can bring its own drivers and steps.
pub fn generate_esp32_embedded(
    board_yaml_path: &Path,
    pipeline_yaml_path: &Path,
    custom_descriptors: Option<&Path>,
) -> Result<String> {
    let scratch = tempfile::tempdir().context("creating temp dir for embedded descriptors")?;
    let drivers_root = scratch.path().join("drivers");
    let steps_root = scratch.path().join("steps");

    extract_descriptors(&EMBEDDED_DRIVERS, &drivers_root).context("extracting driver descriptors")?;
    extract_descriptors(&EMBEDDED_STEPS, &steps_root).context("extracting step descriptors")?;

    if let Some(custom) = custom_descriptors {
        overlay_descriptors(&custom.join("drivers"), &drivers_root)
            .context("overlaying custom driver descriptors")?;
        overlay_descriptors(&custom.join("steps"), &steps_root)
            .context("overlaying custom step descriptors")?;
    }

    generate_esp32(board_yaml_path, pipeline_yaml_path, &drivers_root, &steps_root)
}

/// Writes the `<id>/descriptor.yaml` of every top-level entry in an embedded
/// descriptor tree into `dest`, ignoring everything else the crate carries.
fn extract_descriptors(root: &Dir<'_>, dest: &Path) -> Result<()> {
    for sub in root.dirs() {
        let name = sub
            .path()
            .file_name()
            .and_then(|n| n.to_str())
            .with_context(|| format!("bad descriptor dir name: {}", sub.path().display()))?;
        let rel = format!("{name}/descriptor.yaml");
        if let Some(file) = root.get_file(&rel) {
            let out_dir = dest.join(name);
            std::fs::create_dir_all(&out_dir)
                .with_context(|| format!("creating {}", out_dir.display()))?;
            let out = out_dir.join("descriptor.yaml");
            std::fs::write(&out, file.contents())
                .with_context(|| format!("writing {}", out.display()))?;
        }
    }
    Ok(())
}

/// Copies `<id>/descriptor.yaml` from a filesystem descriptor tree over `dest`.
/// A missing source directory is not an error: a firmware may add only drivers,
/// only steps, or neither.
fn overlay_descriptors(src_root: &Path, dest: &Path) -> Result<()> {
    if !src_root.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(src_root)
        .with_context(|| format!("reading {}", src_root.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let descriptor = entry.path().join("descriptor.yaml");
        if !descriptor.is_file() {
            continue;
        }
        let out_dir = dest.join(entry.file_name());
        std::fs::create_dir_all(&out_dir)
            .with_context(|| format!("creating {}", out_dir.display()))?;
        std::fs::copy(&descriptor, out_dir.join("descriptor.yaml"))
            .with_context(|| format!("copying {}", descriptor.display()))?;
    }
    Ok(())
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
