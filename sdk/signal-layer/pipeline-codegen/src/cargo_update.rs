//! Cargo.toml auto-update for pipeline codegen.
//!
//! When a pipeline is (re)generated this module updates the firmware crate's
//! `Cargo.toml` so that the pipeline's required crates are wired up as a
//! named feature and injected into the selected chip-target feature.
//!
//! ## What is managed
//!
//! - A single `pipeline-{id}` feature in `[features]` that lists the
//!   caller-supplied `extra_feature_deps` (e.g. `"pipeline"` marker) followed
//!   by `dep:<crate>` for every crate derived from the pipeline.
//! - The chip-target feature (e.g. `esp32c6`): stale `pipeline-*` entries are
//!   replaced by the new `pipeline-{id}` — and swept from **every other**
//!   feature list too, so regenerating for a second target never leaves
//!   another chip referencing a feature definition that no longer exists
//!   (which would make the whole workspace unloadable).
//! - Optional dep entries for every required crate are appended to
//!   `[dependencies]` when missing, marked `# auto-added by pipeline-codegen`.
//! - Stale auto-added dep entries that are no longer required are removed.
//!
//! All other content (comments, ordering, hand-written entries) is preserved
//! via `toml_edit`.

use std::path::Path;

use anyhow::Context as _;
use toml_edit::{Array, DocumentMut, Item, Value};

use crate::manifest::BoardManifest;
use crate::pipeline::PipelineFile;

// ── Public API ────────────────────────────────────────────────────────────────

/// Derive the Cargo crate names required by `pipeline` given `manifest`.
///
/// - Each source → `{device.driver}-driver`  (e.g. `bme280-driver`)
/// - Each step   → `{step.op}`               (e.g. `moving-average`)
///
/// The list contains no duplicates and preserves declaration order.
pub fn required_crates(pipeline: &PipelineFile, manifest: &BoardManifest) -> Vec<String> {
    let mut crates: Vec<String> = Vec::new();

    for source in &pipeline.sources {
        let device = manifest
            .devices
            .iter()
            .find(|d| d.id == source.device)
            .unwrap_or_else(|| {
                panic!(
                    "required_crates: source `{}` references unknown device `{}` — \
                     run validate_pipeline_against_manifest before calling this",
                    source.id, source.device
                )
            });
        let name = format!("{}-driver", device.driver);
        if !crates.contains(&name) {
            crates.push(name);
        }
    }

    // Outlet sink devices need their output driver crate too.
    for outlet in &pipeline.outlets {
        let device = manifest
            .devices
            .iter()
            .find(|d| d.id == outlet.device)
            .unwrap_or_else(|| {
                panic!(
                    "required_crates: outlet `{}` references unknown device `{}` — \
                     run validate_pipeline_against_manifest before calling this",
                    outlet.name, outlet.device
                )
            });
        let name = format!("{}-driver", device.driver);
        if !crates.contains(&name) {
            crates.push(name);
        }
    }

    for step in &pipeline.steps {
        let name = step.op.clone();
        if !crates.contains(&name) {
            crates.push(name);
        }
    }

    crates
}

/// Update `cargo_path` (a `Cargo.toml`) for the given pipeline.
///
/// - `pipeline_id`       — pipeline identifier, e.g. `"basic-sensors"`
/// - `required_crates`   — crate names from [`required_crates`]
/// - `extra_feature_deps` — chip-specific deps prepended to the generated
///   feature, e.g. `&["pipeline"]`
/// - `target`            — chip feature to inject into, e.g. `"esp32c6"`
///
/// Returns `true` if the file was modified.
pub fn update(
    cargo_path: &Path,
    pipeline_id: &str,
    required_crates: &[String],
    extra_feature_deps: &[String],
    target: &str,
) -> anyhow::Result<bool> {
    let (doc, changed) = load_and_apply(
        cargo_path,
        pipeline_id,
        required_crates,
        extra_feature_deps,
        target,
    )?;
    if changed {
        std::fs::write(cargo_path, doc.to_string())
            .with_context(|| format!("cannot write '{}'", cargo_path.display()))?;
    }
    Ok(changed)
}

/// Validate that `target` exists as a feature in `cargo_path`.
///
/// Call this before doing any work so a bad `--target` fails fast with a
/// clear message rather than silently producing a broken `Cargo.toml`.
pub fn check_target(cargo_path: &Path, target: &str) -> anyhow::Result<()> {
    let src = std::fs::read_to_string(cargo_path)
        .with_context(|| format!("cannot read '{}'", cargo_path.display()))?;
    let doc: DocumentMut = src
        .parse()
        .with_context(|| format!("cannot parse '{}' as TOML", cargo_path.display()))?;
    validate_target(&doc, target, cargo_path)
}

// ── Internals ─────────────────────────────────────────────────────────────────

fn load_and_apply(
    cargo_path: &Path,
    pipeline_id: &str,
    required_crates: &[String],
    extra_feature_deps: &[String],
    target: &str,
) -> anyhow::Result<(DocumentMut, bool)> {
    let src = std::fs::read_to_string(cargo_path)
        .with_context(|| format!("cannot read '{}'", cargo_path.display()))?;
    let mut doc: DocumentMut = src
        .parse()
        .with_context(|| format!("cannot parse '{}' as TOML", cargo_path.display()))?;

    validate_target(&doc, target, cargo_path)?;

    let feat_name = pipeline_feature_name(pipeline_id);
    let feat_deps = build_feature_deps(required_crates, extra_feature_deps);

    let mut changed = false;
    changed |= ensure_optional_deps(&mut doc, required_crates, cargo_path)?;
    changed |= remove_stale_optional_deps(&mut doc, required_crates);
    changed |= remove_old_pipeline_features(&mut doc, &feat_name);
    changed |= strip_pipeline_refs_from_all_features(&mut doc);
    changed |= insert_pipeline_feature(&mut doc, &feat_name, &feat_deps);
    changed |= update_target_feature(&mut doc, target, &feat_name);

    Ok((doc, changed))
}

fn validate_target(doc: &DocumentMut, target: &str, cargo_path: &Path) -> anyhow::Result<()> {
    let features = doc["features"]
        .as_table()
        .with_context(|| format!("'{}': no [features] table", cargo_path.display()))?;
    if features.get(target).is_none() {
        let available = features
            .iter()
            .map(|(k, _)| k)
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "'{}': target feature '{}' not found in [features] (available: {})",
            cargo_path.display(),
            target,
            available,
        );
    }
    Ok(())
}

/// `basic-sensors` → `pipeline-basic-sensors`
fn pipeline_feature_name(pipeline_id: &str) -> String {
    let slug: String = pipeline_id
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("pipeline-{slug}")
}

/// Build the feature dep list: `[extra..., "dep:crate1", "dep:crate2", ...]`
fn build_feature_deps(required_crates: &[String], extra: &[String]) -> Vec<String> {
    let mut deps: Vec<String> = extra.to_vec();
    deps.extend(required_crates.iter().map(|c| format!("dep:{c}")));
    deps
}

/// Append optional dep entries for any crate not yet present in `[dependencies]`.
/// Entries are marked with `# auto-added by pipeline-codegen` so they can be
/// cleaned up when the pipeline changes.
fn ensure_optional_deps(
    doc: &mut DocumentMut,
    crates: &[String],
    cargo_path: &Path,
) -> anyhow::Result<bool> {
    let deps = doc["dependencies"]
        .as_table_mut()
        .with_context(|| format!("'{}': no [dependencies] table", cargo_path.display()))?;

    let existing: std::collections::HashSet<String> =
        deps.iter().map(|(k, _)| k.replace('-', "_")).collect();

    let mut added = false;
    for name in crates {
        if existing.contains(&name.replace('-', "_")) {
            continue;
        }
        let mut inline = toml_edit::InlineTable::new();
        inline.insert("workspace", toml_edit::Value::from(true));
        inline.insert("optional", toml_edit::Value::from(true));
        deps.insert(name, Item::Value(Value::InlineTable(inline)));
        if let Some(mut key) = deps.key_mut(name) {
            key.leaf_decor_mut()
                .set_prefix("\n# auto-added by pipeline-codegen\n");
        }
        added = true;
    }
    Ok(added)
}

/// Remove auto-added optional deps that are no longer required.
/// Only entries whose key decoration contains the sentinel comment are touched.
fn remove_stale_optional_deps(doc: &mut DocumentMut, crates: &[String]) -> bool {
    let Some(deps) = doc["dependencies"].as_table_mut() else {
        return false;
    };

    let required: std::collections::HashSet<String> =
        crates.iter().map(|c| c.replace('-', "_")).collect();

    let to_remove: Vec<String> = deps
        .iter()
        .filter(|(k, _)| {
            let is_auto = deps
                .key(k)
                .and_then(|key| key.leaf_decor().prefix())
                .and_then(|p| p.as_str())
                .is_some_and(|s| s.contains("auto-added by pipeline-codegen"));
            is_auto && !required.contains(&k.replace('-', "_"))
        })
        .map(|(k, _)| k.to_owned())
        .collect();

    let changed = !to_remove.is_empty();
    for k in to_remove {
        deps.remove(&k);
    }
    changed
}

/// Remove stale `pipeline-*` feature definitions (but not `pipeline` itself).
fn remove_old_pipeline_features(doc: &mut DocumentMut, keep: &str) -> bool {
    let Some(features) = doc["features"].as_table_mut() else {
        return false;
    };

    let to_remove: Vec<String> = features
        .iter()
        .filter(|(k, _)| k.starts_with("pipeline-") && *k != keep)
        .map(|(k, _)| k.to_owned())
        .collect();

    let changed = !to_remove.is_empty();
    for k in to_remove {
        features.remove(&k);
    }
    changed
}

/// Insert or replace the `pipeline-{id}` feature with `feat_deps`.
fn insert_pipeline_feature(doc: &mut DocumentMut, feat_name: &str, feat_deps: &[String]) -> bool {
    let Some(features) = doc["features"].as_table_mut() else {
        return false;
    };

    // Check if identical entry already exists.
    if let Some(existing) = features.get(feat_name)
        && let Some(arr) = existing.as_value().and_then(|v| v.as_array())
    {
        let existing_deps: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
        let new_deps: Vec<&str> = feat_deps.iter().map(String::as_str).collect();
        if existing_deps == new_deps {
            return false;
        }
    }

    let mut arr = Array::new();
    for dep in feat_deps {
        arr.push(dep.as_str());
    }
    features.insert(feat_name, Item::Value(Value::Array(arr)));
    if let Some(mut key) = features.key_mut(feat_name) {
        key.leaf_decor_mut()
            .set_prefix("\n# auto-generated by pipeline-codegen\n");
    }
    true
}

/// Remove every `pipeline-*` reference from every feature list.
///
/// A generated pipeline is wired into exactly one chip target per run; a
/// previous run may have wired it into a *different* chip, and that stale
/// reference survives `remove_old_pipeline_features` (which only removes the
/// feature *definitions*). A reference to an undefined feature makes cargo
/// refuse to load the whole workspace, so the sweep covers all lists — the
/// current target gets its entry re-added by [`update_target_feature`].
fn strip_pipeline_refs_from_all_features(doc: &mut DocumentMut) -> bool {
    let Some(features) = doc["features"].as_table_mut() else {
        return false;
    };

    let mut changed = false;
    for (_key, item) in features.iter_mut() {
        let Some(arr) = item.as_array_mut() else {
            continue;
        };
        let stale: Vec<usize> = arr
            .iter()
            .enumerate()
            .filter_map(|(i, v)| v.as_str()?.starts_with("pipeline-").then_some(i))
            .collect();
        changed |= !stale.is_empty();
        for i in stale.into_iter().rev() {
            arr.remove(i);
        }
    }
    changed
}

/// Inject `feat_name` into the chip-target feature array, removing any stale
/// `pipeline-*` entries first.
fn update_target_feature(doc: &mut DocumentMut, target: &str, feat_name: &str) -> bool {
    let Some(features) = doc["features"].as_table_mut() else {
        return false;
    };
    let Some(arr) = features.get_mut(target).and_then(|i| i.as_array_mut()) else {
        return false;
    };

    let stale: Vec<usize> = arr
        .iter()
        .enumerate()
        .filter_map(|(i, v)| {
            let s = v.as_str()?;
            if s.starts_with("pipeline-") && s != feat_name {
                Some(i)
            } else {
                None
            }
        })
        .collect();

    let mut changed = !stale.is_empty();
    for i in stale.into_iter().rev() {
        arr.remove(i);
    }

    if !arr.iter().any(|v| v.as_str() == Some(feat_name)) {
        arr.push(feat_name);
        changed = true;
    }
    changed
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_manifest_with_devices(drivers: &[&str]) -> BoardManifest {
        use crate::manifest::{BusConfig, BusTransport, DeviceEntry};
        use indexmap::IndexMap;
        let mut buses = IndexMap::new();
        buses.insert(
            "i2c0".to_owned(),
            BusConfig {
                transport: BusTransport::I2c,
                freq_khz: 400,
                pins: [("scl".to_owned(), 10u8), ("sda".to_owned(), 11u8)]
                    .into_iter()
                    .collect(),
                mode: 0,
            },
        );
        let devices = drivers
            .iter()
            .map(|d| DeviceEntry {
                id: d.to_string(),
                driver: d.to_string(),
                bus: "i2c0".to_owned(),
                pins: IndexMap::new(),
                hardware: IndexMap::new(),
            })
            .collect();
        BoardManifest {
            id: "test".into(),
            chip: "test".into(),
            buses,
            gpios: crate::manifest::GpioConfig {
                general_purpose: vec![],
            },
            devices,
        }
    }

    fn make_pipeline(sources: &[&str], step_ops: &[&str]) -> PipelineFile {
        use crate::pipeline::{PipelineInfo, Source, Step};
        PipelineFile {
            pipeline: PipelineInfo {
                id: "basic-sensors".into(),
            },
            sources: sources
                .iter()
                .map(|s| Source {
                    id: s.to_string(),
                    device: s.to_string(),
                    config: Default::default(),
                })
                .collect(),
            steps: step_ops
                .iter()
                .enumerate()
                .map(|(i, op)| Step {
                    id: format!("step{i}"),
                    op: op.to_string(),
                    input: "src.val".into(),
                    config: Default::default(),
                })
                .collect(),
            taps: vec![],
            outlets: vec![],
        }
    }

    #[test]
    fn feature_name_slug() {
        assert_eq!(
            pipeline_feature_name("basic-sensors"),
            "pipeline-basic-sensors"
        );
        assert_eq!(pipeline_feature_name("bme280_demo"), "pipeline-bme280-demo");
    }

    #[test]
    fn required_crates_derives_from_manifest_and_steps() {
        let manifest = make_manifest_with_devices(&["bme280", "veml7700"]);
        let pipeline = make_pipeline(&["bme280", "veml7700"], &["moving-average"]);
        let crates = required_crates(&pipeline, &manifest);
        assert!(crates.contains(&"bme280-driver".to_owned()));
        assert!(crates.contains(&"veml7700-driver".to_owned()));
        assert!(crates.contains(&"moving-average".to_owned()));
        assert_eq!(crates.len(), 3);
    }

    #[test]
    fn no_duplicate_crates() {
        let manifest = make_manifest_with_devices(&["bme280"]);
        let pipeline = make_pipeline(&["bme280", "bme280"], &[]);
        let crates = required_crates(&pipeline, &manifest);
        assert_eq!(
            crates
                .iter()
                .filter(|c| c.as_str() == "bme280-driver")
                .count(),
            1
        );
    }

    #[test]
    fn update_target_feature_removes_stale_and_injects_new() {
        let mut doc: DocumentMut =
            "[features]\nesp32c6 = [\"pipeline-old\", \"wasm-runtime/esp32c6\"]\n"
                .parse()
                .unwrap();
        let changed = update_target_feature(&mut doc, "esp32c6", "pipeline-basic-sensors");
        assert!(changed);
        let arr = doc["features"]["esp32c6"].as_array().unwrap();
        let entries: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
        assert!(entries.contains(&"pipeline-basic-sensors"));
        assert!(!entries.contains(&"pipeline-old"));
        assert!(entries.contains(&"wasm-runtime/esp32c6"));
    }

    #[test]
    fn update_target_feature_idempotent() {
        let mut doc: DocumentMut =
            "[features]\nesp32c6 = [\"wasm-runtime/esp32c6\", \"pipeline-basic-sensors\"]\n"
                .parse()
                .unwrap();
        let changed = update_target_feature(&mut doc, "esp32c6", "pipeline-basic-sensors");
        assert!(!changed);
    }

    #[test]
    fn remove_stale_optional_deps_only_touches_auto_added() {
        let toml = concat!(
            "[dependencies]\n",
            "wasm-runtime = { workspace = true }\n",
            "\n# auto-added by pipeline-codegen\n",
            "old-driver = { workspace = true, optional = true }\n",
            "\n# auto-added by pipeline-codegen\n",
            "bme280-driver = { workspace = true, optional = true }\n",
        );
        let mut doc: DocumentMut = toml.parse().unwrap();
        let changed = remove_stale_optional_deps(&mut doc, &["bme280-driver".to_owned()]);
        assert!(changed);
        let deps = doc["dependencies"].as_table().unwrap();
        assert!(
            deps.get("old-driver").is_none(),
            "stale auto-added dep removed"
        );
        assert!(deps.get("bme280-driver").is_some(), "still-needed dep kept");
        assert!(
            deps.get("wasm-runtime").is_some(),
            "hand-written dep untouched"
        );
    }

    #[test]
    fn hand_written_optional_deps_are_never_removed() {
        let toml = "[dependencies]\nbme280-driver = { workspace = true, optional = true }\n";
        let mut doc: DocumentMut = toml.parse().unwrap();
        let changed = remove_stale_optional_deps(&mut doc, &[]);
        assert!(!changed);
    }

    /// Regression for the second-target regen bug: after regenerating for a
    /// different pipeline/target pair, no chip feature list may reference a
    /// `pipeline-*` feature that is no longer defined — cargo refuses to load
    /// the entire workspace on such a manifest.
    #[test]
    fn second_target_regen_leaves_no_stale_refs() {
        let initial = concat!(
            "[features]\n",
            "default = [\"esp32c6\"]\n",
            "esp32c5 = [\"esp-hal/esp32c5\", \"hang-record\"]\n",
            "esp32c6 = [\"esp-hal/esp32c6\", \"hang-record\"]\n",
            "esp32c61 = [\"esp-hal/esp32c61\", \"hang-record\"]\n",
            "hang-record = []\n",
            "pipeline = []\n",
            "\n",
            "[dependencies]\n",
            "esp-hal = { workspace = true }\n",
        );
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        std::fs::write(tmp.path(), initial).expect("write initial");

        // Run 1: basic-sensors on esp32c5.
        update(
            tmp.path(),
            "basic-sensors",
            &["bme280-driver".to_owned()],
            &["pipeline".to_owned()],
            "esp32c5",
        )
        .expect("first regen");

        // Run 2: actuators-demo on esp32c6.
        update(
            tmp.path(),
            "actuators-demo",
            &["gpio-output-driver".to_owned()],
            &["pipeline".to_owned()],
            "esp32c6",
        )
        .expect("second regen");

        let out = std::fs::read_to_string(tmp.path()).expect("read result");
        let doc: DocumentMut = out.parse().expect("result must stay valid TOML");
        let features = doc["features"].as_table().expect("features table");

        // Exactly one generated pipeline feature is defined.
        let defined: Vec<&str> = features
            .iter()
            .map(|(k, _)| k)
            .filter(|k| k.starts_with("pipeline-"))
            .collect();
        assert_eq!(defined, ["pipeline-actuators-demo"]);

        // Every pipeline-* reference in any feature list points at it.
        for (key, item) in features.iter() {
            let Some(arr) = item.as_array() else { continue };
            for v in arr.iter().filter_map(|v| v.as_str()) {
                if v.starts_with("pipeline-") {
                    assert_eq!(
                        (key, v),
                        ("esp32c6", "pipeline-actuators-demo"),
                        "stale pipeline reference `{v}` left in feature `{key}`"
                    );
                }
            }
        }
    }

    #[test]
    fn required_crates_panics_on_unknown_device() {
        use crate::pipeline::{PipelineInfo, Source};
        let manifest = make_manifest_with_devices(&["bme280"]);
        let pipeline = PipelineFile {
            pipeline: PipelineInfo { id: "p".into() },
            sources: vec![Source {
                id: "s".into(),
                device: "nonexistent_device".into(),
                config: Default::default(),
            }],
            steps: vec![],
            taps: vec![],
            outlets: vec![],
        };
        let result = std::panic::catch_unwind(|| required_crates(&pipeline, &manifest));
        assert!(
            result.is_err(),
            "required_crates must panic when source references an unknown device"
        );
    }
}
