use indexmap::IndexMap;

use crate::descriptor::{DriverSchema, Scope};
use crate::manifest::BoardManifest;
use crate::pipeline::{PipelineFile, TapKind};

// `ValidationError` is now defined in `pipeline-backend-api`; re-export it so
// all existing `crate::validate::ValidationError` paths keep compiling.
pub use pipeline_backend_api::ValidationError;

/// Returns `Ok(())` if `s` — after normalising hyphens to underscores — is a
/// valid Rust identifier, or an `Err` message suitable for a [`ValidationError`].
/// Identifiers must be non-empty, start with a letter or `_`, and contain only
/// letters, digits, and `_`/`-`.
pub fn validate_rust_ident(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err("identifier must not be empty".into());
    }
    let normalized = s.replace('-', "_");
    let mut chars = normalized.chars();
    let first = chars.next().unwrap();
    if !first.is_alphabetic() && first != '_' {
        return Err(format!(
            "`{s}` is not a valid identifier: must start with a letter or underscore"
        ));
    }
    if let Some(bad) = chars.find(|c| !c.is_alphanumeric() && *c != '_') {
        return Err(format!(
            "`{s}` is not a valid identifier: illegal character `{bad}`"
        ));
    }
    // Reject Rust keywords (e.g. `type`, `match`, `move`) that the character-level
    // check above would otherwise accept — codegen calls `Ident::new(...)` on the
    // result, which panics on keywords. `syn::parse_str` returns Err for any
    // reserved word.
    if syn::parse_str::<syn::Ident>(&normalized).is_err() {
        return Err(format!(
            "`{s}` is not a valid identifier: `{normalized}` is a reserved Rust keyword"
        ));
    }
    Ok(())
}

/// Capacity of the tap registry in the Signal Layer runtime.
/// Must stay in sync with `signal_layer_core::MAX_TAPS`.
const MAX_TAPS: usize = 16;

/// Number of taps automatically injected by codegen (e.g. `_signal_layer_health`).
const AUTO_TAPS: usize = 1;

/// Capacity of the outlet registry in the Signal Layer runtime.
/// Must stay in sync with `signal_layer_core::MAX_OUTLETS`.
const MAX_OUTLETS: usize = 8;

/// Rust primitive types that `config_value_tokens` (and therefore the generated
/// firmware) can handle. Every `rust_type:` field in a driver descriptor must
/// appear in this list.
const KNOWN_RUST_TYPES: &[&str] = &["u8", "u16", "u32", "u64", "usize", "f32", "f64", "bool"];

/// Check that a YAML value is compatible with the declared `rust_type`.
/// `pointer_width` (32 or 64) is used to bound `usize` checks against the target.
fn validate_config_value(
    value: &serde_yaml::Value,
    rust_type: &str,
    pointer_width: u32,
) -> Result<(), String> {
    match rust_type {
        "u8" => check_uint(value, u64::from(u8::MAX), "u8"),
        "u16" => check_uint(value, u64::from(u16::MAX), "u16"),
        "u32" => check_uint(value, u64::from(u32::MAX), "u32"),
        "u64" => check_uint(value, u64::MAX, "u64"),
        "usize" if pointer_width >= 64 => check_uint(value, u64::MAX, "usize"),
        "usize" => check_uint(value, u64::from(u32::MAX), "usize (32-bit target)"),
        "f32" | "f64" => value
            .as_f64()
            .map(|_| ())
            .ok_or_else(|| format!("expected float, got {}", yaml_kind(value))),
        "bool" => value
            .as_bool()
            .map(|_| ())
            .ok_or_else(|| format!("expected bool, got {}", yaml_kind(value))),
        // Non-primitive type → enum variant: value must be a valid Rust identifier string.
        enum_type => {
            let s = value.as_str().ok_or_else(|| {
                format!(
                    "expected a variant name (string) for enum type `{enum_type}`, got {}",
                    yaml_kind(value)
                )
            })?;
            syn::parse_str::<syn::Ident>(s).map(|_| ()).map_err(|_| {
                format!("`{s}` is not a valid variant name for enum type `{enum_type}`")
            })
        }
    }
}

fn check_uint(value: &serde_yaml::Value, max: u64, type_name: &str) -> Result<(), String> {
    if let Some(n) = value.as_i64() {
        if n < 0 {
            return Err(format!("{n} is negative, cannot fit in {type_name}"));
        }
        if n.cast_unsigned() > max {
            return Err(format!("{n} does not fit in {type_name} (max {max})"));
        }
        Ok(())
    } else if let Some(n) = value.as_u64() {
        if n > max {
            return Err(format!("{n} does not fit in {type_name} (max {max})"));
        }
        Ok(())
    } else {
        Err(format!(
            "expected integer for {type_name}, got {}",
            yaml_kind(value)
        ))
    }
}

fn yaml_kind(v: &serde_yaml::Value) -> &'static str {
    match v {
        serde_yaml::Value::Null => "null",
        serde_yaml::Value::Bool(_) => "boolean",
        serde_yaml::Value::Number(_) => "number",
        serde_yaml::Value::String(_) => "string",
        serde_yaml::Value::Sequence(_) => "sequence",
        serde_yaml::Value::Mapping(_) => "mapping",
        serde_yaml::Value::Tagged(_) => "tagged",
    }
}

/// Validate a pipeline against a board manifest, driver schemas (keyed by driver
/// id) and step schemas (keyed by op id). When a schema is absent, the checks
/// that depend on it are skipped.
///
/// `pointer_width` must match the target's pointer size (32 or 64 bits) — used
/// to range-check `usize` config fields. Pass `backend.pointer_width()`.
#[allow(clippy::too_many_lines)] // validation: many independent checks, not meaningfully splittable
pub fn validate_pipeline_against_manifest(
    pipeline: &PipelineFile,
    manifest: &BoardManifest,
    driver_schemas: &IndexMap<String, DriverSchema>,
    step_schemas: &IndexMap<String, DriverSchema>,
    pointer_width: u32,
) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    // Build device lookup: id → DeviceEntry
    let device_map: IndexMap<&str, &crate::manifest::DeviceEntry> = manifest
        .devices
        .iter()
        .map(|d| (d.id.as_str(), d))
        .collect();

    // Validate the pipeline id first (BLOCKER 1): it is emitted unescaped into
    // generated main.rs (println! and identifier position) and Cargo.toml
    // (crate name and [[bin]] name). A crafted id like `x`);evil` injects
    // arbitrary Rust; a `"` injects arbitrary TOML.
    if let Err(e) = validate_rust_ident(&pipeline.pipeline.id) {
        errors.push(ValidationError::new(format!("pipeline id {e}")));
    }

    // All user-supplied identifiers must be valid Rust identifiers before codegen
    // can safely call Ident::new() on their derived names.
    for src in &pipeline.sources {
        if let Err(e) = validate_rust_ident(&src.id) {
            errors.push(ValidationError::new(format!("source id {e}")));
        }
    }
    for step in &pipeline.steps {
        if let Err(e) = validate_rust_ident(&step.id) {
            errors.push(ValidationError::new(format!("step id {e}")));
        }
        if let Err(e) = validate_rust_ident(&step.op) {
            errors.push(ValidationError::new(format!("step `{}`: op {e}", step.id)));
        }
    }
    for tap in &pipeline.taps {
        if let Err(e) = validate_rust_ident(&tap.name) {
            errors.push(ValidationError::new(format!("tap name {e}")));
        }
    }

    // Enforce registry capacity — codegen auto-injects AUTO_TAPS additional taps
    // only when the pipeline has at least one source (the `_signal_layer_health`
    // tap is per-pipeline, gated by source presence). A source-less pipeline
    // therefore gets the full MAX_TAPS budget.
    let reserved = if pipeline.sources.is_empty() {
        0
    } else {
        AUTO_TAPS
    };
    let user_tap_limit = MAX_TAPS - reserved;
    if pipeline.taps.len() > user_tap_limit {
        errors.push(ValidationError::new(format!(
            "pipeline defines {} taps but the registry holds at most {} \
             ({} reserved for codegen-injected taps); reduce to ≤ {}",
            pipeline.taps.len(),
            MAX_TAPS,
            reserved,
            user_tap_limit,
        )));
    }

    // Names starting with `_` are reserved for codegen-injected taps.
    for tap in &pipeline.taps {
        if tap.name.starts_with('_') {
            errors.push(ValidationError::new(format!(
                "tap `{}`: names starting with `_` are reserved for codegen-injected taps",
                tap.name
            )));
        }
    }

    // Duplicate tap names would emit duplicate statics and double-register in the registry.
    let mut seen_taps: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for tap in &pipeline.taps {
        if !seen_taps.insert(tap.name.as_str()) {
            errors.push(ValidationError::new(format!(
                "duplicate tap name `{}`",
                tap.name
            )));
        }
    }

    // Check for duplicate source ids in the pipeline.
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for src in &pipeline.sources {
        if !seen.insert(src.id.as_str()) {
            errors.push(ValidationError::new(format!(
                "duplicate source id `{}`",
                src.id
            )));
        }
    }

    for src in &pipeline.sources {
        // Each source's `device:` must exist in the manifest.
        let Some(device) = device_map.get(src.device.as_str()) else {
            errors.push(ValidationError::new(format!(
                "source `{}`: references unknown device `{}`",
                src.id, src.device
            )));
            continue;
        };

        // If a schema is available for this driver, validate config scopes and bus needs.
        if let Some(schema) = driver_schemas.get(device.driver.as_str()) {
            // Pipeline config must not contain hardware-scope fields.
            for key in src.config.keys() {
                if schema
                    .config_schema
                    .get(key.as_str())
                    .is_some_and(|f| f.scope == Scope::Hardware)
                {
                    errors.push(ValidationError::new(format!(
                        "source `{}`: field `{key}` has scope `hardware` and must be set \
                         in the board manifest, not the pipeline",
                        src.id
                    )));
                }
            }

            // Board manifest hardware block must not contain application-scope fields.
            for key in device.hardware.keys() {
                if schema
                    .config_schema
                    .get(key.as_str())
                    .is_some_and(|f| f.scope == Scope::Application)
                {
                    errors.push(ValidationError::new(format!(
                        "device `{}`: field `{key}` has scope `application` and must be \
                         set in the pipeline, not the board manifest",
                        device.id
                    )));
                }
            }

            // The bus the device is wired to must satisfy the driver's bus requirement.
            if let Some(bus) = manifest.buses.get(&device.bus) {
                for req in &schema.requires.buses {
                    if req.transport != bus.transport {
                        errors.push(ValidationError::new(format!(
                            "source `{}`: driver `{}` requires a {:?} bus, but device `{}` is \
                             wired to bus `{}` ({:?})",
                            src.id,
                            device.driver,
                            req.transport,
                            device.id,
                            device.bus,
                            bus.transport
                        )));
                    }
                }
            }

            // Each named pin on the device must be declared in the driver's
            // `optional_pins` — otherwise the codegen would silently drop it.
            // The `cs` pin is special-cased: it's supplied by the SPI bus
            // wiring, not by the driver descriptor.
            let bus_is_spi = manifest
                .buses
                .get(&device.bus)
                .is_some_and(|b| b.transport == crate::manifest::BusTransport::Spi);
            for pin_name in device.pins.keys() {
                if bus_is_spi && pin_name == "cs" {
                    continue;
                }
                if !schema.requires.optional_pins.iter().any(|n| n == pin_name) {
                    errors.push(ValidationError::new(format!(
                        "device `{}`: pin `{pin_name}` is not declared in driver `{}`'s \
                         `optional_pins`",
                        device.id, device.driver
                    )));
                }
            }

            // Validate rust_type declarations and check config values against them.
            for (field_name, field_def) in &schema.config_schema {
                if let Some(rust_type) = &field_def.rust_type {
                    let is_primitive = KNOWN_RUST_TYPES.contains(&rust_type.as_str());
                    // Primitive check: unknown types are only rejected when they are
                    // NOT a valid identifier, i.e. they can't be an enum type name at all.
                    // Valid-ident non-primitives are treated as enum types.
                    if !is_primitive {
                        if syn::parse_str::<syn::Ident>(rust_type).is_err() {
                            errors.push(ValidationError::new(format!(
                                "driver `{}`: field `{field_name}` declares invalid \
                                 rust_type `{rust_type}` (not a primitive and not a valid identifier)",
                                device.driver
                            )));
                            continue;
                        }
                        // Validate the descriptor default for enum fields so a typo'd
                        // default is caught here rather than producing invalid generated code.
                        if let Err(e) =
                            validate_config_value(&field_def.default, rust_type, pointer_width)
                        {
                            errors.push(ValidationError::new(format!(
                                "driver `{}`: field `{field_name}` default: {e}",
                                device.driver
                            )));
                        }
                    }
                    // Check user-provided values — hardware fields come from the
                    // manifest, application fields from the pipeline config.
                    let value_opt = match field_def.scope {
                        Scope::Hardware => device.hardware.get(field_name.as_str()),
                        Scope::Application => src.config.get(field_name.as_str()),
                    };
                    if let Some(value) = value_opt
                        && let Err(e) = validate_config_value(value, rust_type, pointer_width)
                    {
                        let location = match field_def.scope {
                            Scope::Hardware => format!("device `{}`", device.id),
                            Scope::Application => format!("source `{}`", src.id),
                        };
                        errors.push(ValidationError::new(format!(
                            "{location}: field `{field_name}`: {e}"
                        )));
                    }
                }
            }
        }
    }

    for step in &pipeline.steps {
        if let Some(schema) = step_schemas.get(step.op.as_str()) {
            for key in step.config.keys() {
                if !schema.config_schema.contains_key(key.as_str()) {
                    errors.push(ValidationError::new(format!(
                        "step `{}` (op `{}`): unknown config key `{key}`",
                        step.id, step.op
                    )));
                }
            }
        }
    }

    // ── Outlets (write side) ────────────────────────────────────────────────
    // Outlets live in their own registry/namespace, so their names are checked
    // independently of taps.
    // Only cell-driven outlets (no feed-forward `input`) occupy the outlet
    // registry; pipeline-driven outlets are applied inline and never registered.
    let cell_outlet_count = pipeline
        .outlets
        .iter()
        .filter(|o| o.input.is_none())
        .count();
    if cell_outlet_count > MAX_OUTLETS {
        errors.push(ValidationError::new(format!(
            "pipeline defines {cell_outlet_count} cell-driven outlets but the registry \
             holds at most {MAX_OUTLETS}",
        )));
    }
    let mut seen_outlets: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut driven_devices: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for outlet in &pipeline.outlets {
        if let Err(e) = validate_rust_ident(&outlet.name) {
            errors.push(ValidationError::new(format!("outlet name {e}")));
        }
        if outlet.name.starts_with('_') {
            errors.push(ValidationError::new(format!(
                "outlet `{}`: names starting with `_` are reserved",
                outlet.name
            )));
        }
        if !seen_outlets.insert(outlet.name.as_str()) {
            errors.push(ValidationError::new(format!(
                "duplicate outlet name `{}`",
                outlet.name
            )));
        }
        // Single-writer (F1): at most one outlet may drive a given device.
        if !driven_devices.insert(outlet.device.as_str()) {
            errors.push(ValidationError::new(format!(
                "outlet `{}`: device `{}` is already driven by another outlet \
                 (single-writer per device)",
                outlet.name, outlet.device
            )));
        }
        // The target device must exist and be output-capable, and the outlet's
        // declared type must match the driver's declared command type.
        let Some(device) = device_map.get(outlet.device.as_str()) else {
            errors.push(ValidationError::new(format!(
                "outlet `{}`: references unknown device `{}`",
                outlet.name, outlet.device
            )));
            continue;
        };
        if let Some(schema) = driver_schemas.get(device.driver.as_str()) {
            match &schema.writes {
                None => errors.push(ValidationError::new(format!(
                    "outlet `{}`: device `{}` (driver `{}`) is not output-capable \
                     (no `writes` in its descriptor)",
                    outlet.name, outlet.device, device.driver
                ))),
                Some(write) if write.command_type != outlet.type_name => {
                    errors.push(ValidationError::new(format!(
                        "outlet `{}`: declared type `{}`, but driver `{}` consumes `{}`",
                        outlet.name, outlet.type_name, device.driver, write.command_type
                    )));
                }
                Some(_) => {}
            }

            // Pipeline (outlet) config must not contain hardware-scope fields.
            for key in outlet.config.keys() {
                if schema
                    .config_schema
                    .get(key.as_str())
                    .is_some_and(|f| f.scope == Scope::Hardware)
                {
                    errors.push(ValidationError::new(format!(
                        "outlet `{}`: field `{key}` has scope `hardware` and must be set \
                         in the board manifest, not the pipeline",
                        outlet.name
                    )));
                }
            }

            // Board manifest hardware block must not contain application-scope fields.
            for key in device.hardware.keys() {
                if schema
                    .config_schema
                    .get(key.as_str())
                    .is_some_and(|f| f.scope == Scope::Application)
                {
                    errors.push(ValidationError::new(format!(
                        "device `{}`: field `{key}` has scope `application` and must be \
                         set in the pipeline, not the board manifest",
                        device.id
                    )));
                }
            }
        }

        // Feed-forward input (pipeline-driven outlet): the referenced producer
        // must exist and its output type must equal the outlet's command type.
        if let Some(input) = &outlet.input {
            match producer_type(input, pipeline, manifest, driver_schemas, step_schemas) {
                Some(actual) if actual != outlet.type_name => {
                    errors.push(ValidationError::new(format!(
                        "outlet `{}`: feed-forward input `{}` produces `{}`, but the outlet's \
                         command type is `{}`",
                        outlet.name, input, actual, outlet.type_name
                    )));
                }
                Some(_) => {}
                None => {
                    errors.push(ValidationError::new(format!(
                        "outlet `{}`: feed-forward input `{}` does not resolve to a known \
                         source field or step output",
                        outlet.name, input
                    )));
                }
            }
        }
    }

    // A source id and an outlet name must not collide: `<id>.<field>` would be
    // ambiguous (producer_type resolves sources first, tap validation outlets).
    for outlet in &pipeline.outlets {
        if pipeline.sources.iter().any(|s| s.id == outlet.name) {
            errors.push(ValidationError::new(format!(
                "outlet `{}` collides with a source of the same id; `{}.<field>` is ambiguous",
                outlet.name, outlet.name
            )));
        }
    }

    // Outlet feedback taps (#1018): a status read-back tap `<outlet>.<field>`
    // must be a Retained slot and name a real status output of the driver.
    for tap in &pipeline.taps {
        let Some((owner, field)) = tap.source.split_once('.') else {
            continue;
        };
        let Some(outlet) = pipeline.outlets.iter().find(|o| o.name == owner) else {
            continue;
        };
        if outlet.input.is_some() {
            errors.push(ValidationError::new(format!(
                "tap `{}`: outlet `{}` is feed-forward (has `input:`) and has no sink task, \
                 so it cannot publish feedback taps",
                tap.name, owner
            )));
            continue;
        }
        if field == OUTLET_ERROR_FIELD {
            if tap.kind != TapKind::Event {
                errors.push(ValidationError::new(format!(
                    "tap `{}`: outlet error tap `{}` must be `kind: event`",
                    tap.name, tap.source
                )));
            }
            continue; // type is enforced by check_type_compatibility
        }
        if tap.kind != TapKind::Retained {
            errors.push(ValidationError::new(format!(
                "tap `{}`: outlet status read-back `{}` must be `kind: retained`",
                tap.name, tap.source
            )));
        }
        let field_exists = manifest
            .devices
            .iter()
            .find(|d| d.id == outlet.device)
            .and_then(|d| driver_schemas.get(d.driver.as_str()))
            .is_some_and(|s| s.outputs.iter().any(|o| o.name == field));
        if !field_exists {
            errors.push(ValidationError::new(format!(
                "tap `{}`: outlet `{}` driver declares no status output `{}`",
                tap.name, owner, field
            )));
        }
    }

    // At most one `<outlet>.error` tap per outlet — the sink task wires exactly one.
    for outlet in &pipeline.outlets {
        let err_src = format!("{}.{OUTLET_ERROR_FIELD}", outlet.name);
        let count = pipeline.taps.iter().filter(|t| t.source == err_src).count();
        if count > 1 {
            errors.push(ValidationError::new(format!(
                "outlet `{}`: {count} `.error` taps declared; only one is supported",
                outlet.name
            )));
        }
    }

    errors.extend(check_type_compatibility(
        pipeline,
        manifest,
        driver_schemas,
        step_schemas,
    ));

    errors.extend(check_step_graph_cycles(pipeline));

    errors
}

/// Detect cycles in the processing step graph using Kahn's algorithm.
/// Each step has exactly one input, which is either a source field
/// (`"source_id.field"`) or another step's id. A remaining non-zero in-degree
/// after the sort means the step is part of a cycle.
fn check_step_graph_cycles(pipeline: &PipelineFile) -> Vec<ValidationError> {
    use std::collections::{HashMap, HashSet, VecDeque};

    let step_ids: HashSet<&str> = pipeline.steps.iter().map(|n| n.id.as_str()).collect();

    // Edges go from dependency → dependent (dependency must be processed first).
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut in_degree: HashMap<&str, usize> = HashMap::new();

    for step in &pipeline.steps {
        in_degree.entry(step.id.as_str()).or_insert(0);
        if !step.input.contains('.') {
            let dep_id = step.input.as_str();
            if step_ids.contains(dep_id) {
                dependents.entry(dep_id).or_default().push(step.id.as_str());
                *in_degree.entry(step.id.as_str()).or_insert(0) += 1;
            }
        }
    }

    let mut queue: VecDeque<&str> = in_degree
        .iter()
        .filter(|&(_, deg)| *deg == 0)
        .map(|(&id, _)| id)
        .collect();

    let mut processed = 0;
    while let Some(id) = queue.pop_front() {
        processed += 1;
        if let Some(deps) = dependents.get(id) {
            for &dep in deps {
                let deg = in_degree.get_mut(dep).unwrap();
                *deg -= 1;
                if *deg == 0 {
                    queue.push_back(dep);
                }
            }
        }
    }

    if processed < pipeline.steps.len() {
        let mut cycle_steps: Vec<&str> = in_degree
            .iter()
            .filter(|&(_, deg)| *deg > 0)
            .map(|(&id, _)| id)
            .collect();
        cycle_steps.sort_unstable();
        return vec![ValidationError::new(format!(
            "processing step graph contains a cycle involving step(s): {}",
            cycle_steps.join(", ")
        ))];
    }

    vec![]
}

/// Reserved status field on an outlet: its error Event tap payload type.
pub(crate) const OUTLET_ERROR_FIELD: &str = "error";
pub(crate) const OUTLET_ERROR_TYPE: &str = "OutletFault";

/// Resolve a pipeline reference to the value type it produces:
/// - `"<source>.<field>"` — a sensor driver output,
/// - `"<outlet>.<field>"` — a hybrid output driver's status read-back (or the
///   reserved `"<outlet>.error"` → `OutletFault`),
/// - `"<step_id>"` — a processing step's output.
///
/// Returns `None` when the reference or its schema cannot be resolved.
fn producer_type(
    reference: &str,
    pipeline: &PipelineFile,
    manifest: &BoardManifest,
    driver_schemas: &IndexMap<String, DriverSchema>,
    step_schemas: &IndexMap<String, DriverSchema>,
) -> Option<String> {
    producer_type_bounded(
        reference,
        pipeline,
        manifest,
        driver_schemas,
        step_schemas,
        pipeline.steps.len() + 1,
    )
}

fn producer_type_bounded(
    reference: &str,
    pipeline: &PipelineFile,
    manifest: &BoardManifest,
    driver_schemas: &IndexMap<String, DriverSchema>,
    step_schemas: &IndexMap<String, DriverSchema>,
    depth: usize,
) -> Option<String> {
    if depth == 0 {
        return None; // guard against a step-input cycle
    }
    if let Some((owner_id, field)) = reference.split_once('.') {
        // A source's sensor output …
        if let Some(src) = pipeline.sources.iter().find(|s| s.id == owner_id) {
            let device = manifest.devices.iter().find(|d| d.id == src.device)?;
            let schema = driver_schemas.get(device.driver.as_str())?;
            return schema
                .outputs
                .iter()
                .find(|o| o.name == field)
                .map(|o| o.type_name.clone());
        }
        // … or a hybrid outlet's status read-back / error event.
        if let Some(outlet) = pipeline.outlets.iter().find(|o| o.name == owner_id) {
            if field == OUTLET_ERROR_FIELD {
                return Some(OUTLET_ERROR_TYPE.to_string());
            }
            let device = manifest.devices.iter().find(|d| d.id == outlet.device)?;
            let schema = driver_schemas.get(device.driver.as_str())?;
            return schema
                .outputs
                .iter()
                .find(|o| o.name == field)
                .map(|o| o.type_name.clone());
        }
        None
    } else {
        let step = pipeline.steps.iter().find(|n| n.id == reference)?;
        let schema = step_schemas.get(step.op.as_str())?;
        match schema.outputs.first() {
            Some(o) => Some(o.type_name.clone()),
            // Type-transparent step (e.g. cadence): no declared output — its
            // output type is whatever feeds it, so walk upstream.
            None => producer_type_bounded(
                &step.input,
                pipeline,
                manifest,
                driver_schemas,
                step_schemas,
                depth - 1,
            ),
        }
    }
}

/// Check that each step's declared input type matches the type of the value
/// feeding it, and that each tap's declared type matches its source's output.
fn check_type_compatibility(
    pipeline: &PipelineFile,
    manifest: &BoardManifest,
    driver_schemas: &IndexMap<String, DriverSchema>,
    step_schemas: &IndexMap<String, DriverSchema>,
) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    for step in &pipeline.steps {
        let Some(schema) = step_schemas.get(step.op.as_str()) else {
            continue;
        };
        let Some(input) = schema.inputs.first() else {
            continue;
        };
        let Some(actual) = producer_type(
            &step.input,
            pipeline,
            manifest,
            driver_schemas,
            step_schemas,
        ) else {
            continue;
        };
        if actual != input.type_name {
            errors.push(ValidationError::new(format!(
                "step `{}` (op `{}`) expects input type `{}`, but `{}` produces `{}`",
                step.id, step.op, input.type_name, step.input, actual
            )));
        }
    }

    for tap in &pipeline.taps {
        let Some(actual) = producer_type(
            &tap.source,
            pipeline,
            manifest,
            driver_schemas,
            step_schemas,
        ) else {
            continue;
        };
        if actual != tap.type_name {
            errors.push(ValidationError::new(format!(
                "tap `{}`: declared type `{}`, but source `{}` produces `{}`",
                tap.name, tap.type_name, tap.source, actual
            )));
        }
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::ConfigField;
    use crate::manifest::parse_manifest;
    use crate::pipeline::{
        Outlet, PipelineFile, PipelineInfo, Source, Step, Tap, TapKind, TapStreamKind,
    };

    fn schema_with(fields: &[(&str, Scope)]) -> DriverSchema {
        let config_schema = fields
            .iter()
            .map(|(name, scope)| {
                (
                    name.to_string(),
                    ConfigField {
                        scope: scope.clone(),
                        rust_type: None,
                        default: serde_yaml::Value::Null,
                    },
                )
            })
            .collect();
        DriverSchema {
            config_schema,
            ..Default::default()
        }
    }

    fn simple_manifest() -> BoardManifest {
        parse_manifest(
            r"
id: test-board
chip: esp32c6
buses:
  i2c0:
    transport: i2c
    pins:
      scl: 10
      sda: 11
    freq_khz: 400
gpios:
  general_purpose: [0, 1, 2, 3, 4, 5, 6, 7, 12, 13]
devices:
  - id: bme280
    driver: bme280
    bus: i2c0
    hardware:
      i2c_addr: 0x76
  - id: ads1115
    driver: ads1115
    bus: i2c0
    hardware:
      i2c_addr: 0x48
",
        )
        .unwrap()
    }

    fn pipeline_with_source(device: &str, config_keys: &[(&str, &str)]) -> PipelineFile {
        PipelineFile {
            pipeline: PipelineInfo { id: "test".into() },
            sources: vec![Source {
                id: "src".into(),
                device: device.to_string(),
                config: config_keys
                    .iter()
                    .map(|(k, v)| (k.to_string(), serde_yaml::Value::String(v.to_string())))
                    .collect(),
            }],
            steps: vec![],
            taps: vec![],
            outlets: vec![],
        }
    }

    #[test]
    fn valid_pipeline_passes() {
        let manifest = simple_manifest();
        let pipeline = pipeline_with_source("bme280", &[("sample_interval_ms", "1000")]);
        let mut schemas = IndexMap::new();
        schemas.insert(
            "bme280".into(),
            schema_with(&[
                ("sample_interval_ms", Scope::Application),
                ("i2c_addr", Scope::Hardware),
            ]),
        );
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &schemas,
            &IndexMap::new(),
            32,
        );
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    }

    #[test]
    fn unknown_device_is_rejected() {
        let manifest = simple_manifest();
        let pipeline = pipeline_with_source("nonexistent_sensor", &[]);
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &IndexMap::new(),
            &IndexMap::new(),
            32,
        );
        assert!(
            errors.iter().any(|e| e.message.contains("unknown device")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn hardware_scope_field_in_pipeline_is_rejected() {
        let manifest = simple_manifest();
        // Pipeline sets i2c_addr which is scope: hardware
        let pipeline = pipeline_with_source("ads1115", &[("i2c_addr", "0x48")]);
        let mut schemas = IndexMap::new();
        schemas.insert(
            "ads1115".into(),
            schema_with(&[("i2c_addr", Scope::Hardware)]),
        );
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &schemas,
            &IndexMap::new(),
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("i2c_addr") && e.message.contains("hardware")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn application_scope_field_in_manifest_hardware_block_is_rejected() {
        // Create a manifest where a device's hardware block contains an
        // application-scoped field (sample_interval_ms).
        let manifest = parse_manifest(
            r"
id: test-board
chip: esp32c6
buses:
  i2c0:
    transport: i2c
    pins:
      scl: 10
      sda: 11
    freq_khz: 400
gpios:
  general_purpose: [0, 1, 2]
devices:
  - id: bme280
    driver: bme280
    bus: i2c0
    hardware:
      sample_interval_ms: 1000
",
        )
        .unwrap();
        let pipeline = pipeline_with_source("bme280", &[]);
        let mut schemas = IndexMap::new();
        schemas.insert(
            "bme280".into(),
            schema_with(&[("sample_interval_ms", Scope::Application)]),
        );
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &schemas,
            &IndexMap::new(),
            32,
        );
        assert!(
            errors.iter().any(|e| {
                e.message.contains("sample_interval_ms") && e.message.contains("application")
            }),
            "got: {errors:?}"
        );
    }

    fn driver_schema(
        outputs: &[(&str, &str)],
        requires_transport: Option<crate::manifest::BusTransport>,
    ) -> DriverSchema {
        use crate::descriptor::{DriverOutput, RequiredBus, Requires};
        DriverSchema {
            outputs: outputs
                .iter()
                .map(|(name, ty)| DriverOutput {
                    name: name.to_string(),
                    type_name: ty.to_string(),
                    unit: String::new(),
                })
                .collect(),
            requires: Requires {
                buses: requires_transport
                    .map(|transport| vec![RequiredBus { transport }])
                    .unwrap_or_default(),
                optional_pins: vec![],
            },
            ..Default::default()
        }
    }

    fn step_schema(input_ty: &str, output_ty: &str) -> DriverSchema {
        use crate::descriptor::{DriverInput, DriverOutput};
        DriverSchema {
            inputs: vec![DriverInput {
                name: "value".into(),
                type_name: input_ty.into(),
            }],
            outputs: vec![DriverOutput {
                name: "out".into(),
                type_name: output_ty.into(),
                unit: String::new(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn bus_requirement_not_met_is_rejected() {
        // Device is wired to an I2C bus, but the driver requires SPI.
        let manifest = simple_manifest();
        let pipeline = pipeline_with_source("bme280", &[]);
        let mut schemas = IndexMap::new();
        schemas.insert(
            "bme280".into(),
            driver_schema(&[], Some(crate::manifest::BusTransport::Spi)),
        );
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &schemas,
            &IndexMap::new(),
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("requires a Spi bus")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn bus_requirement_met_passes() {
        let manifest = simple_manifest();
        let pipeline = pipeline_with_source("bme280", &[]);
        let mut schemas = IndexMap::new();
        schemas.insert(
            "bme280".into(),
            driver_schema(&[], Some(crate::manifest::BusTransport::I2c)),
        );
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &schemas,
            &IndexMap::new(),
            32,
        );
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    }

    #[test]
    fn tap_type_mismatch_is_rejected() {
        let manifest = simple_manifest();
        let mut pipeline = pipeline_with_source("bme280", &[]);
        pipeline.taps.push(Tap {
            name: "temp".into(),
            kind: TapKind::Retained,
            type_name: "u32".into(), // declared u32 …
            source: "src.temperature".into(),
            stream_kind: TapStreamKind::Metric,
        });
        let mut schemas = IndexMap::new();
        // … but the driver output is f32.
        schemas.insert(
            "bme280".into(),
            driver_schema(&[("temperature", "f32")], None),
        );
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &schemas,
            &IndexMap::new(),
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("tap `temp`") && e.message.contains("u32")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn unknown_step_config_key_is_rejected() {
        let manifest = simple_manifest();
        let mut pipeline = pipeline_with_source("bme280", &[]);
        pipeline.steps.push(Step {
            id: "avg".into(),
            op: "moving-average".into(),
            input: "src.temperature".into(),
            config: {
                let mut m = IndexMap::new();
                m.insert("windwo".into(), serde_yaml::Value::Number(8.into())); // typo
                m
            },
        });
        let mut step_schemas = IndexMap::new();
        step_schemas.insert(
            "moving-average".into(),
            schema_with(&[("window", Scope::Application)]),
        );
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &IndexMap::new(),
            &step_schemas,
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("step `avg`") && e.message.contains("`windwo`")),
            "got: {errors:?}"
        );
    }

    fn driver_schema_with_optional_pins(pins: &[&str]) -> DriverSchema {
        use crate::descriptor::Requires;
        DriverSchema {
            requires: Requires {
                buses: vec![],
                optional_pins: pins.iter().map(std::string::ToString::to_string).collect(),
            },
            ..Default::default()
        }
    }

    #[test]
    fn undeclared_device_pin_is_rejected() {
        // Manifest wires `nint` and `reset`, but the driver descriptor only
        // declares `nint` — `reset` must be flagged.
        let manifest = parse_manifest(
            r"
id: test-board
chip: esp32c6
buses:
  i2c0:
    transport: i2c
    pins: { scl: 10, sda: 11 }
    freq_khz: 400
gpios:
  general_purpose: [0, 1, 2, 3, 4, 5]
devices:
  - id: ccs811
    driver: ccs811
    bus: i2c0
    pins:
      nint: 4
      reset: 5
",
        )
        .unwrap();
        let pipeline = PipelineFile {
            pipeline: PipelineInfo { id: "test".into() },
            sources: vec![Source {
                id: "aq".into(),
                device: "ccs811".into(),
                config: IndexMap::new(),
            }],
            steps: vec![],
            taps: vec![],
            outlets: vec![],
        };
        let mut schemas = IndexMap::new();
        schemas.insert("ccs811".into(), driver_schema_with_optional_pins(&["nint"]));
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &schemas,
            &IndexMap::new(),
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("`reset`") && e.message.contains("optional_pins")),
            "got: {errors:?}"
        );
        // `nint` IS declared — it must not be flagged.
        assert!(
            !errors.iter().any(|e| e.message.contains("`nint`")),
            "declared pin should not be flagged: {errors:?}"
        );
    }

    #[test]
    fn spi_cs_pin_is_always_allowed() {
        // `cs` is wired by the SPI bus layer, not declared in optional_pins,
        // so it must pass validation without an explicit descriptor entry.
        let manifest = parse_manifest(
            r"
id: test-board
chip: esp32c6
buses:
  spi2:
    transport: spi
    pins: { sclk: 0, mosi: 1, miso: 2 }
    freq_khz: 1000
gpios:
  general_purpose: [3, 4]
devices:
  - id: dev
    driver: somedrv
    bus: spi2
    pins:
      cs: 3
",
        )
        .unwrap();
        let pipeline = PipelineFile {
            pipeline: PipelineInfo { id: "test".into() },
            sources: vec![Source {
                id: "src".into(),
                device: "dev".into(),
                config: IndexMap::new(),
            }],
            steps: vec![],
            taps: vec![],
            outlets: vec![],
        };
        let mut schemas = IndexMap::new();
        schemas.insert("somedrv".into(), driver_schema_with_optional_pins(&[]));
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &schemas,
            &IndexMap::new(),
            32,
        );
        assert!(
            !errors.iter().any(|e| e.message.contains("`cs`")),
            "cs on SPI device must be permitted: {errors:?}"
        );
    }

    #[test]
    fn step_input_type_mismatch_is_rejected() {
        let manifest = simple_manifest();
        let mut pipeline = pipeline_with_source("bme280", &[]);
        pipeline.steps.push(Step {
            id: "avg".into(),
            op: "moving-average".into(),
            input: "src.temperature".into(),
            config: IndexMap::new(),
        });
        let mut driver_schemas = IndexMap::new();
        // Driver output is u32, but the step consumes f32.
        driver_schemas.insert(
            "bme280".into(),
            driver_schema(&[("temperature", "u32")], None),
        );
        let mut step_schemas = IndexMap::new();
        step_schemas.insert("moving-average".into(), step_schema("f32", "f32"));
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &driver_schemas,
            &step_schemas,
            32,
        );
        assert!(
            errors.iter().any(|e| e.message.contains("step `avg`")
                && e.message.contains("expects input type `f32`")),
            "got: {errors:?}"
        );
    }

    // ── identifier sanity tests ──────────────────────────────────────────────

    #[test]
    fn validate_rust_ident_accepts_valid_names() {
        assert!(validate_rust_ident("temperature").is_ok());
        assert!(validate_rust_ident("_priv").is_ok());
        assert!(validate_rust_ident("my_sensor").is_ok());
        assert!(
            validate_rust_ident("moving-average").is_ok(),
            "hyphens are normalised to _"
        );
        assert!(validate_rust_ident("sensor2").is_ok());
    }

    #[test]
    fn validate_rust_ident_rejects_empty() {
        assert!(validate_rust_ident("").is_err());
    }

    #[test]
    fn validate_rust_ident_rejects_leading_digit() {
        let err = validate_rust_ident("7sensor").unwrap_err();
        assert!(err.contains("must start with"), "got: {err}");
    }

    #[test]
    fn validate_rust_ident_rejects_illegal_char() {
        let err = validate_rust_ident("foo bar").unwrap_err();
        assert!(err.contains("illegal character"), "got: {err}");
        assert!(validate_rust_ident("foo.bar").is_err());
    }

    #[test]
    fn validate_rust_ident_rejects_rust_keyword() {
        let err = validate_rust_ident("type").unwrap_err();
        assert!(err.contains("reserved Rust keyword"), "got: {err}");
        assert!(validate_rust_ident("match").is_err());
        assert!(validate_rust_ident("move").is_err());
    }

    fn pipeline_with_ids(source_id: &str, step_id: &str, tap_name: &str) -> PipelineFile {
        PipelineFile {
            pipeline: PipelineInfo { id: "test".into() },
            sources: vec![Source {
                id: source_id.into(),
                device: "bme280".into(),
                config: IndexMap::new(),
            }],
            steps: vec![Step {
                id: step_id.into(),
                op: "moving-average".into(),
                input: format!("{source_id}.temperature"),
                config: IndexMap::new(),
            }],
            taps: vec![Tap {
                name: tap_name.into(),
                kind: TapKind::Retained,
                type_name: "f32".into(),
                source: format!("{source_id}.temperature"),
                stream_kind: TapStreamKind::Metric,
            }],
            outlets: vec![],
        }
    }

    #[test]
    fn digit_leading_source_id_is_rejected() {
        let manifest = simple_manifest();
        let pipeline = pipeline_with_ids("7sensor", "avg", "temp");
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &IndexMap::new(),
            &IndexMap::new(),
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("source id") && e.message.contains("7sensor")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn digit_leading_step_id_is_rejected() {
        let manifest = simple_manifest();
        let pipeline = pipeline_with_ids("src", "3way", "temp");
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &IndexMap::new(),
            &IndexMap::new(),
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("step id") && e.message.contains("3way")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn digit_leading_tap_name_is_rejected() {
        let manifest = simple_manifest();
        let pipeline = pipeline_with_ids("src", "avg", "9temp");
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &IndexMap::new(),
            &IndexMap::new(),
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("tap name") && e.message.contains("9temp")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn space_in_source_id_is_rejected() {
        let manifest = simple_manifest();
        let pipeline = pipeline_with_ids("my sensor", "avg", "temp");
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &IndexMap::new(),
            &IndexMap::new(),
            32,
        );
        assert!(
            errors.iter().any(|e| e.message.contains("source id")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn hyphenated_source_id_passes_validation() {
        // Hyphens normalise to underscores — the resulting ident is valid.
        let manifest = simple_manifest();
        let mut pipeline = pipeline_with_source("bme280", &[]);
        pipeline.sources[0].id = "my-sensor".into();
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &IndexMap::new(),
            &IndexMap::new(),
            32,
        );
        assert!(
            !errors
                .iter()
                .any(|e| e.message.contains("my-sensor") && e.message.contains("identifier")),
            "hyphenated id should be valid: {errors:?}"
        );
    }

    // ── tap registry hygiene tests ──────────────────────────────────────────

    fn tap(name: &str) -> Tap {
        Tap {
            name: name.into(),
            kind: TapKind::Retained,
            type_name: "f32".into(),
            source: "src.temperature".into(),
            stream_kind: TapStreamKind::Metric,
        }
    }

    fn pipeline_with_taps(taps: Vec<Tap>) -> PipelineFile {
        PipelineFile {
            pipeline: PipelineInfo { id: "test".into() },
            sources: vec![Source {
                id: "src".into(),
                device: "bme280".into(),
                config: IndexMap::new(),
            }],
            steps: vec![],
            taps,
            outlets: vec![],
        }
    }

    #[test]
    fn duplicate_tap_names_are_rejected() {
        let manifest = simple_manifest();
        let pipeline = pipeline_with_taps(vec![tap("temp"), tap("temp")]);
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &IndexMap::new(),
            &IndexMap::new(),
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("duplicate tap name") && e.message.contains("temp")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn unique_tap_names_pass() {
        let manifest = simple_manifest();
        let pipeline = pipeline_with_taps(vec![tap("temperature"), tap("humidity")]);
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &IndexMap::new(),
            &IndexMap::new(),
            32,
        );
        assert!(
            !errors.iter().any(|e| e.message.contains("duplicate")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn reserved_prefix_tap_is_rejected() {
        let manifest = simple_manifest();
        let pipeline = pipeline_with_taps(vec![tap("_signal_layer_health")]);
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &IndexMap::new(),
            &IndexMap::new(),
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("_signal_layer_health")
                    && e.message.contains("reserved")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn any_underscore_prefix_tap_is_rejected() {
        let manifest = simple_manifest();
        let pipeline = pipeline_with_taps(vec![tap("_private")]);
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &IndexMap::new(),
            &IndexMap::new(),
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("_private") && e.message.contains("reserved")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn tap_count_at_limit_passes() {
        let manifest = simple_manifest();
        let taps: Vec<Tap> = (0..(MAX_TAPS - AUTO_TAPS))
            .map(|i| tap(&format!("tap{i}")))
            .collect();
        let pipeline = pipeline_with_taps(taps);
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &IndexMap::new(),
            &IndexMap::new(),
            32,
        );
        assert!(
            !errors.iter().any(|e| e.message.contains("registry")),
            "exactly {}-AUTO_TAPS taps should pass: {errors:?}",
            MAX_TAPS
        );
    }

    #[test]
    fn tap_count_over_limit_is_rejected() {
        let manifest = simple_manifest();
        let taps: Vec<Tap> = (0..MAX_TAPS) // one over the user limit
            .map(|i| tap(&format!("tap{i}")))
            .collect();
        let pipeline = pipeline_with_taps(taps);
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &IndexMap::new(),
            &IndexMap::new(),
            32,
        );
        assert!(
            errors.iter().any(|e| e.message.contains("registry")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn source_less_pipeline_can_use_full_capacity() {
        // No sources → no auto-injected _signal_layer_health tap → full MAX_TAPS available.
        let manifest = simple_manifest();
        let taps: Vec<Tap> = (0..MAX_TAPS).map(|i| tap(&format!("tap{i}"))).collect();
        let pipeline = PipelineFile {
            pipeline: PipelineInfo { id: "test".into() },
            sources: vec![],
            steps: vec![],
            taps,
            outlets: vec![],
        };
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &IndexMap::new(),
            &IndexMap::new(),
            32,
        );
        assert!(
            !errors.iter().any(|e| e.message.contains("registry")),
            "got: {errors:?}"
        );
    }

    // ── config value validation tests (F2, F3, F4, F5) ──────────────────────

    fn schema_with_hardware_field(rust_type: &str) -> DriverSchema {
        schema_with_hardware_field_and_default(rust_type, serde_yaml::Value::Number(0.into()))
    }

    fn schema_with_hardware_field_and_default(
        rust_type: &str,
        default: serde_yaml::Value,
    ) -> DriverSchema {
        use crate::descriptor::ConfigField;
        let mut config_schema = IndexMap::new();
        config_schema.insert(
            "addr".to_string(),
            ConfigField {
                scope: Scope::Hardware,
                rust_type: Some(rust_type.to_string()),
                default,
            },
        );
        DriverSchema {
            config_schema,
            ..Default::default()
        }
    }

    fn manifest_with_hardware_value(value: &serde_yaml::Value) -> BoardManifest {
        parse_manifest(&format!(
            r"
id: test-board
chip: esp32c6
buses:
  i2c0:
    transport: i2c
    pins: {{scl: 10, sda: 11}}
    freq_khz: 400
gpios:
  general_purpose: [0, 1, 2, 3, 4, 5, 6, 7, 12, 13]
devices:
  - id: sensor
    driver: testdrv
    bus: i2c0
    hardware:
      addr: {value}
",
            value = serde_yaml::to_string(&value).unwrap().trim()
        ))
        .unwrap()
    }

    fn pipeline_for_sensor() -> PipelineFile {
        PipelineFile {
            pipeline: PipelineInfo { id: "test".into() },
            sources: vec![Source {
                id: "src".into(),
                device: "sensor".into(),
                config: IndexMap::new(),
            }],
            steps: vec![],
            taps: vec![],
            outlets: vec![],
        }
    }

    fn check_hardware(
        rust_type: &str,
        value: &serde_yaml::Value,
        pointer_width: u32,
    ) -> Vec<ValidationError> {
        let manifest = manifest_with_hardware_value(value);
        let pipeline = pipeline_for_sensor();
        let mut schemas = IndexMap::new();
        schemas.insert("testdrv".to_string(), schema_with_hardware_field(rust_type));
        validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &schemas,
            &IndexMap::new(),
            pointer_width,
        )
    }

    fn check_hardware_with_default(
        rust_type: &str,
        default: serde_yaml::Value,
        value: &serde_yaml::Value,
        pointer_width: u32,
    ) -> Vec<ValidationError> {
        let manifest = manifest_with_hardware_value(value);
        let pipeline = pipeline_for_sensor();
        let mut schemas = IndexMap::new();
        schemas.insert(
            "testdrv".to_string(),
            schema_with_hardware_field_and_default(rust_type, default),
        );
        validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &schemas,
            &IndexMap::new(),
            pointer_width,
        )
    }

    #[test]
    fn u8_valid_value_passes() {
        let errors = check_hardware("u8", &serde_yaml::Value::Number(100.into()), 32);
        assert!(
            !errors.iter().any(|e| e.message.contains("addr")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn u8_overflow_is_rejected() {
        let errors = check_hardware("u8", &serde_yaml::Value::Number(256.into()), 32);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("addr") && e.message.contains("256")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn u8_negative_is_rejected() {
        let errors = check_hardware("u8", &serde_yaml::Value::Number((-1).into()), 32);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("addr") && e.message.contains("negative")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn u8_string_is_rejected() {
        let errors = check_hardware("u8", &serde_yaml::Value::String("abc".into()), 32);
        assert!(
            errors.iter().any(|e| e.message.contains("addr")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn u32_max_passes() {
        let errors = check_hardware(
            "u32",
            &serde_yaml::Value::Number(u64::from(u32::MAX).into()),
            32,
        );
        assert!(
            !errors.iter().any(|e| e.message.contains("addr")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn usize_within_u32_range_passes_on_32bit() {
        let errors = check_hardware("usize", &serde_yaml::Value::Number(65535u64.into()), 32);
        assert!(
            !errors.iter().any(|e| e.message.contains("addr")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn usize_over_u32_max_rejected_on_32bit() {
        let over = u64::from(u32::MAX) + 1;
        let errors = check_hardware("usize", &serde_yaml::Value::Number(over.into()), 32);
        assert!(
            errors.iter().any(|e| e.message.contains("addr")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn usize_over_u32_max_passes_on_64bit() {
        let over = u64::from(u32::MAX) + 1;
        let errors = check_hardware("usize", &serde_yaml::Value::Number(over.into()), 64);
        assert!(
            !errors.iter().any(|e| e.message.contains("addr")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn invalid_rust_type_identifier_in_schema_is_rejected() {
        // A rust_type with illegal characters (e.g. spaces, dots) is rejected.
        let errors = check_hardware("complex type", &serde_yaml::Value::String("X".into()), 32);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("invalid rust_type")
                    || e.message.contains("not a primitive")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn enum_field_with_valid_variant_string_passes() {
        // A non-primitive rust_type (enum) with a valid identifier string value passes.
        let errors = check_hardware_with_default(
            "MeasMode",
            serde_yaml::Value::String("Every1s".into()),
            &serde_yaml::Value::String("Every1s".into()),
            32,
        );
        assert!(
            !errors.iter().any(|e| e.message.contains("addr")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn enum_field_with_numeric_value_is_rejected() {
        // An enum field must have a string (variant name) value, not a number.
        let errors = check_hardware_with_default(
            "MeasMode",
            serde_yaml::Value::String("Every1s".into()),
            &serde_yaml::Value::Number(1.into()),
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("addr") && e.message.contains("variant name")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn enum_field_with_invalid_ident_string_is_rejected() {
        // An enum field value that is not a valid Rust identifier is rejected.
        let errors = check_hardware_with_default(
            "MeasMode",
            serde_yaml::Value::String("Every1s".into()),
            &serde_yaml::Value::String("not valid".into()),
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("addr")
                    && e.message.contains("not a valid variant name")),
            "got: {errors:?}"
        );
    }

    // ── processing step graph cycle detection tests ────────────────────────────────

    fn pipeline_with_steps(steps: Vec<Step>) -> PipelineFile {
        PipelineFile {
            pipeline: PipelineInfo { id: "test".into() },
            sources: vec![Source {
                id: "src".into(),
                device: "bme280".into(),
                config: IndexMap::new(),
            }],
            steps,
            taps: vec![],
            outlets: vec![],
        }
    }

    #[test]
    fn linear_step_chain_passes() {
        let manifest = simple_manifest();
        let pipeline = pipeline_with_steps(vec![
            Step {
                id: "a".into(),
                op: "op".into(),
                input: "src.temperature".into(),
                config: IndexMap::new(),
            },
            Step {
                id: "b".into(),
                op: "op".into(),
                input: "a".into(),
                config: IndexMap::new(),
            },
            Step {
                id: "c".into(),
                op: "op".into(),
                input: "b".into(),
                config: IndexMap::new(),
            },
        ]);
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &IndexMap::new(),
            &IndexMap::new(),
            32,
        );
        assert!(
            !errors.iter().any(|e| e.message.contains("cycle")),
            "linear chain should not trigger cycle detection: {errors:?}"
        );
    }

    #[test]
    fn self_loop_step_is_rejected() {
        let manifest = simple_manifest();
        let pipeline = pipeline_with_steps(vec![Step {
            id: "loop".into(),
            op: "op".into(),
            input: "loop".into(),
            config: IndexMap::new(),
        }]);
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &IndexMap::new(),
            &IndexMap::new(),
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("cycle") && e.message.contains("loop")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn two_step_cycle_is_rejected() {
        let manifest = simple_manifest();
        let pipeline = pipeline_with_steps(vec![
            Step {
                id: "a".into(),
                op: "op".into(),
                input: "b".into(),
                config: IndexMap::new(),
            },
            Step {
                id: "b".into(),
                op: "op".into(),
                input: "a".into(),
                config: IndexMap::new(),
            },
        ]);
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &IndexMap::new(),
            &IndexMap::new(),
            32,
        );
        assert!(
            errors.iter().any(|e| e.message.contains("cycle")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn three_step_cycle_is_rejected() {
        let manifest = simple_manifest();
        let pipeline = pipeline_with_steps(vec![
            Step {
                id: "a".into(),
                op: "op".into(),
                input: "c".into(),
                config: IndexMap::new(),
            },
            Step {
                id: "b".into(),
                op: "op".into(),
                input: "a".into(),
                config: IndexMap::new(),
            },
            Step {
                id: "c".into(),
                op: "op".into(),
                input: "b".into(),
                config: IndexMap::new(),
            },
        ]);
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &IndexMap::new(),
            &IndexMap::new(),
            32,
        );
        assert!(
            errors.iter().any(|e| e.message.contains("cycle")),
            "got: {errors:?}"
        );
    }

    // ── outlet (write side) validation tests ────────────────────────────────

    fn writer_schema(command_type: &str) -> DriverSchema {
        use crate::descriptor::{DriverWrite, OutputMode};
        DriverSchema {
            writes: Some(DriverWrite {
                command_type: command_type.into(),
                mode: OutputMode::Digital,
            }),
            ..Default::default()
        }
    }

    fn manifest_with_output_device() -> BoardManifest {
        parse_manifest(
            r"
id: test-board
chip: esp32c6
buses:
  i2c0:
    transport: i2c
    pins: { scl: 10, sda: 11 }
    freq_khz: 400
gpios:
  general_purpose: [0, 1, 2, 3, 4, 5]
devices:
  - id: relay1
    driver: gpio-output
    pins:
      out: 5
  - id: bme280
    driver: bme280
    bus: i2c0
",
        )
        .unwrap()
    }

    fn pipeline_with_outlets(outlets: Vec<Outlet>) -> PipelineFile {
        PipelineFile {
            pipeline: PipelineInfo { id: "test".into() },
            sources: vec![],
            steps: vec![],
            taps: vec![],
            outlets,
        }
    }

    fn outlet(name: &str, type_name: &str, device: &str) -> Outlet {
        Outlet {
            name: name.into(),
            type_name: type_name.into(),
            device: device.into(),
            input: None,
            config: IndexMap::new(),
        }
    }

    #[test]
    fn valid_outlet_passes() {
        let manifest = manifest_with_output_device();
        let pipeline = pipeline_with_outlets(vec![outlet("relay1_cmd", "DigitalState", "relay1")]);
        let mut schemas = IndexMap::new();
        schemas.insert("gpio-output".into(), writer_schema("DigitalState"));
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &schemas,
            &IndexMap::new(),
            32,
        );
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    }

    #[test]
    fn outlet_unknown_device_is_rejected() {
        let manifest = manifest_with_output_device();
        let pipeline = pipeline_with_outlets(vec![outlet("cmd", "DigitalState", "nope")]);
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &IndexMap::new(),
            &IndexMap::new(),
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("unknown device") && e.message.contains("nope")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn outlet_on_non_output_device_is_rejected() {
        let manifest = manifest_with_output_device();
        // bme280 is a sensor — its schema has no `writes`.
        let pipeline = pipeline_with_outlets(vec![outlet("cmd", "DigitalState", "bme280")]);
        let mut schemas = IndexMap::new();
        schemas.insert(
            "bme280".into(),
            driver_schema(&[("temperature", "f32")], None),
        );
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &schemas,
            &IndexMap::new(),
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("not output-capable")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn outlet_type_mismatch_is_rejected() {
        let manifest = manifest_with_output_device();
        // Driver consumes DigitalState, but the outlet declares PwmDuty.
        let pipeline = pipeline_with_outlets(vec![outlet("cmd", "PwmDuty", "relay1")]);
        let mut schemas = IndexMap::new();
        schemas.insert("gpio-output".into(), writer_schema("DigitalState"));
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &schemas,
            &IndexMap::new(),
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("cmd") && e.message.contains("PwmDuty")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn duplicate_outlet_names_are_rejected() {
        let manifest = manifest_with_output_device();
        let pipeline = pipeline_with_outlets(vec![
            outlet("cmd", "DigitalState", "relay1"),
            outlet("cmd", "DigitalState", "bme280"),
        ]);
        let mut schemas = IndexMap::new();
        schemas.insert("gpio-output".into(), writer_schema("DigitalState"));
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &schemas,
            &IndexMap::new(),
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("duplicate outlet name")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn two_outlets_driving_one_device_is_rejected() {
        let manifest = manifest_with_output_device();
        let pipeline = pipeline_with_outlets(vec![
            outlet("cmd_a", "DigitalState", "relay1"),
            outlet("cmd_b", "DigitalState", "relay1"),
        ]);
        let mut schemas = IndexMap::new();
        schemas.insert("gpio-output".into(), writer_schema("DigitalState"));
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &schemas,
            &IndexMap::new(),
            32,
        );
        assert!(
            errors.iter().any(|e| e.message.contains("single-writer")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn reserved_prefix_outlet_is_rejected() {
        let manifest = manifest_with_output_device();
        let pipeline = pipeline_with_outlets(vec![outlet("_hidden", "DigitalState", "relay1")]);
        let mut schemas = IndexMap::new();
        schemas.insert("gpio-output".into(), writer_schema("DigitalState"));
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &schemas,
            &IndexMap::new(),
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("_hidden") && e.message.contains("reserved")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn outlet_count_over_limit_is_rejected() {
        let manifest = manifest_with_output_device();
        let outlets = (0..=MAX_OUTLETS)
            .map(|i| outlet(&format!("cmd{i}"), "DigitalState", "relay1"))
            .collect();
        let pipeline = pipeline_with_outlets(outlets);
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &IndexMap::new(),
            &IndexMap::new(),
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("registry holds at most")),
            "got: {errors:?}"
        );
    }

    /// Output-capable schema (consumes `DigitalState`) with scoped config fields.
    fn output_schema(fields: &[(&str, Scope)]) -> DriverSchema {
        use crate::descriptor::{DriverWrite, OutputMode};
        DriverSchema {
            writes: Some(DriverWrite {
                command_type: "DigitalState".into(),
                mode: OutputMode::Digital,
            }),
            ..schema_with(fields)
        }
    }

    #[test]
    fn hardware_scope_field_in_outlet_config_is_rejected() {
        let manifest = manifest_with_output_device();
        // Outlet config sets `active_low`, which is scope: hardware.
        let mut relay = outlet("relay", "DigitalState", "relay1");
        relay
            .config
            .insert("active_low".into(), serde_yaml::Value::Bool(true));
        let pipeline = pipeline_with_outlets(vec![relay]);
        let mut schemas = IndexMap::new();
        schemas.insert(
            "gpio-output".into(),
            output_schema(&[("active_low", Scope::Hardware)]),
        );
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &schemas,
            &IndexMap::new(),
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("active_low") && e.message.contains("hardware")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn application_scope_field_in_output_device_hardware_is_rejected() {
        // Output device's hardware block sets `write_interval_ms`, scope: application.
        let manifest = parse_manifest(
            r"
id: test-board
chip: esp32c6
buses:
  i2c0:
    transport: i2c
    pins: { scl: 10, sda: 11 }
    freq_khz: 400
gpios:
  general_purpose: [0, 1, 2, 3, 4, 5]
devices:
  - id: relay1
    driver: gpio-output
    pins:
      out: 5
    hardware:
      write_interval_ms: 100
",
        )
        .unwrap();
        let pipeline = pipeline_with_outlets(vec![outlet("relay", "DigitalState", "relay1")]);
        let mut schemas = IndexMap::new();
        schemas.insert(
            "gpio-output".into(),
            output_schema(&[("write_interval_ms", Scope::Application)]),
        );
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &schemas,
            &IndexMap::new(),
            32,
        );
        assert!(
            errors.iter().any(|e| {
                e.message.contains("write_interval_ms") && e.message.contains("application")
            }),
            "got: {errors:?}"
        );
    }

    fn feed_forward_pipeline() -> PipelineFile {
        PipelineFile {
            pipeline: PipelineInfo { id: "t".into() },
            sources: vec![Source {
                id: "src".into(),
                device: "bme280".into(),
                config: IndexMap::new(),
            }],
            steps: vec![Step {
                id: "ctrl".into(),
                op: "hysteresis".into(),
                input: "src.temperature".into(),
                config: IndexMap::new(),
            }],
            taps: vec![],
            outlets: vec![Outlet {
                name: "relay1_cmd".into(),
                type_name: "DigitalState".into(),
                device: "relay1".into(),
                input: Some("ctrl".into()),
                config: IndexMap::new(),
            }],
        }
    }

    fn feed_forward_schemas(
        step_output: &str,
    ) -> (
        IndexMap<String, DriverSchema>,
        IndexMap<String, DriverSchema>,
    ) {
        let mut drivers = IndexMap::new();
        drivers.insert("gpio-output".into(), writer_schema("DigitalState"));
        drivers.insert(
            "bme280".into(),
            driver_schema(&[("temperature", "f32")], None),
        );
        let mut steps = IndexMap::new();
        steps.insert("hysteresis".into(), step_schema("f32", step_output));
        (drivers, steps)
    }

    #[test]
    fn feed_forward_outlet_type_match_passes() {
        let manifest = manifest_with_output_device();
        let pipeline = feed_forward_pipeline();
        let (drivers, steps) = feed_forward_schemas("DigitalState");
        let errors = validate_pipeline_against_manifest(&pipeline, &manifest, &drivers, &steps, 32);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    }

    #[test]
    fn feed_forward_outlet_type_mismatch_is_rejected() {
        let manifest = manifest_with_output_device();
        // The controller step produces PwmDuty, but the outlet is DigitalState.
        let pipeline = feed_forward_pipeline();
        let (drivers, steps) = feed_forward_schemas("PwmDuty");
        let errors = validate_pipeline_against_manifest(&pipeline, &manifest, &drivers, &steps, 32);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("feed-forward input")
                    && e.message.contains("DigitalState")),
            "got: {errors:?}"
        );
    }

    fn hybrid_manifest() -> BoardManifest {
        parse_manifest(
            r"
id: test-board
chip: esp32c6
buses:
  i2c0:
    transport: i2c
    pins: { scl: 10, sda: 11 }
    freq_khz: 400
gpios:
  general_purpose: [0, 1, 2, 3, 4, 5]
devices:
  - id: relay1
    driver: gpio-output-feedback
    pins:
      out: 2
      feedback: 3
",
        )
        .unwrap()
    }

    fn hybrid_schema() -> DriverSchema {
        use crate::descriptor::{DriverOutput, DriverWrite, OutputMode, Requires};
        DriverSchema {
            writes: Some(DriverWrite {
                command_type: "DigitalState".into(),
                mode: OutputMode::Digital,
            }),
            outputs: vec![DriverOutput {
                name: "contact".into(),
                type_name: "bool".into(),
                unit: String::new(),
            }],
            requires: Requires {
                buses: vec![],
                optional_pins: vec!["out".into(), "feedback".into()],
            },
            ..Default::default()
        }
    }

    #[test]
    fn hybrid_outlet_status_and_error_taps_type_check() {
        let manifest = hybrid_manifest();
        let mut pipeline =
            pipeline_with_outlets(vec![outlet("heat_relay", "DigitalState", "relay1")]);
        pipeline.taps = vec![
            Tap {
                name: "relay_contact".into(),
                kind: TapKind::Retained,
                type_name: "bool".into(),
                source: "heat_relay.contact".into(),
                stream_kind: TapStreamKind::Metric,
            },
            Tap {
                name: "relay_fault".into(),
                kind: TapKind::Event,
                type_name: "OutletFault".into(),
                source: "heat_relay.error".into(),
                stream_kind: TapStreamKind::Metric,
            },
        ];
        let mut schemas = IndexMap::new();
        schemas.insert("gpio-output-feedback".into(), hybrid_schema());
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &schemas,
            &IndexMap::new(),
            32,
        );
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    }

    #[test]
    fn hybrid_status_tap_type_mismatch_is_rejected() {
        let manifest = hybrid_manifest();
        let mut pipeline =
            pipeline_with_outlets(vec![outlet("heat_relay", "DigitalState", "relay1")]);
        // Status field `contact` is bool, but the tap declares f32.
        pipeline.taps = vec![Tap {
            name: "relay_contact".into(),
            kind: TapKind::Retained,
            type_name: "f32".into(),
            source: "heat_relay.contact".into(),
            stream_kind: TapStreamKind::Metric,
        }];
        let mut schemas = IndexMap::new();
        schemas.insert("gpio-output-feedback".into(), hybrid_schema());
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &schemas,
            &IndexMap::new(),
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("relay_contact") && e.message.contains("bool")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn outlet_status_tap_wrong_kind_is_rejected() {
        let manifest = hybrid_manifest();
        let mut pipeline =
            pipeline_with_outlets(vec![outlet("heat_relay", "DigitalState", "relay1")]);
        // A status read-back must be Retained; Event here is invalid.
        pipeline.taps = vec![Tap {
            name: "relay_contact".into(),
            kind: TapKind::Event,
            type_name: "bool".into(),
            source: "heat_relay.contact".into(),
            stream_kind: TapStreamKind::Metric,
        }];
        let mut schemas = IndexMap::new();
        schemas.insert("gpio-output-feedback".into(), hybrid_schema());
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &schemas,
            &IndexMap::new(),
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("relay_contact") && e.message.contains("retained")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn outlet_status_tap_unknown_field_is_rejected() {
        let manifest = hybrid_manifest();
        let mut pipeline =
            pipeline_with_outlets(vec![outlet("heat_relay", "DigitalState", "relay1")]);
        // `missing` is not a declared driver output.
        pipeline.taps = vec![Tap {
            name: "relay_missing".into(),
            kind: TapKind::Retained,
            type_name: "bool".into(),
            source: "heat_relay.missing".into(),
            stream_kind: TapStreamKind::Metric,
        }];
        let mut schemas = IndexMap::new();
        schemas.insert("gpio-output-feedback".into(), hybrid_schema());
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &schemas,
            &IndexMap::new(),
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("relay_missing") && e.message.contains("status output")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn outlet_error_tap_wrong_kind_is_rejected() {
        let manifest = hybrid_manifest();
        let mut pipeline =
            pipeline_with_outlets(vec![outlet("heat_relay", "DigitalState", "relay1")]);
        // The reserved `.error` tap must be Event; Retained here is invalid.
        pipeline.taps = vec![Tap {
            name: "relay_fault".into(),
            kind: TapKind::Retained,
            type_name: "OutletFault".into(),
            source: "heat_relay.error".into(),
            stream_kind: TapStreamKind::Metric,
        }];
        let mut schemas = IndexMap::new();
        schemas.insert("gpio-output-feedback".into(), hybrid_schema());
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &schemas,
            &IndexMap::new(),
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("relay_fault") && e.message.contains("event")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn outlet_multiple_error_taps_are_rejected() {
        let manifest = hybrid_manifest();
        let mut pipeline =
            pipeline_with_outlets(vec![outlet("heat_relay", "DigitalState", "relay1")]);
        pipeline.taps = vec![
            Tap {
                name: "relay_fault_a".into(),
                kind: TapKind::Event,
                type_name: "OutletFault".into(),
                source: "heat_relay.error".into(),
                stream_kind: TapStreamKind::Metric,
            },
            Tap {
                name: "relay_fault_b".into(),
                kind: TapKind::Event,
                type_name: "OutletFault".into(),
                source: "heat_relay.error".into(),
                stream_kind: TapStreamKind::Metric,
            },
        ];
        let mut schemas = IndexMap::new();
        schemas.insert("gpio-output-feedback".into(), hybrid_schema());
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &schemas,
            &IndexMap::new(),
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("only one is supported")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn feedback_tap_on_feed_forward_outlet_is_rejected() {
        let manifest = hybrid_manifest();
        let mut ff_outlet = outlet("heat_relay", "DigitalState", "relay1");
        ff_outlet.input = Some("ctrl".into()); // feed-forward: no sink task
        let mut pipeline = pipeline_with_outlets(vec![ff_outlet]);
        pipeline.taps = vec![Tap {
            name: "relay_contact".into(),
            kind: TapKind::Retained,
            type_name: "bool".into(),
            source: "heat_relay.contact".into(),
            stream_kind: TapStreamKind::Metric,
        }];
        let mut schemas = IndexMap::new();
        schemas.insert("gpio-output-feedback".into(), hybrid_schema());
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &schemas,
            &IndexMap::new(),
            32,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("relay_contact") && e.message.contains("sink task")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn source_outlet_name_collision_is_rejected() {
        let manifest = hybrid_manifest();
        let mut pipeline =
            pipeline_with_outlets(vec![outlet("relay1_ctrl", "DigitalState", "relay1")]);
        // A source shares the outlet's name → `relay1_ctrl.<field>` is ambiguous.
        pipeline.sources = vec![Source {
            id: "relay1_ctrl".into(),
            device: "relay1".into(),
            config: IndexMap::new(),
        }];
        let mut schemas = IndexMap::new();
        schemas.insert("gpio-output-feedback".into(), hybrid_schema());
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &schemas,
            &IndexMap::new(),
            32,
        );
        assert!(
            errors.iter().any(|e| e.message.contains("collides")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn feed_forward_outlet_does_not_count_against_registry() {
        // MAX_OUTLETS+1 outlets, but all feed-forward (input set) -> none registered.
        let manifest = manifest_with_output_device();
        let outlets = (0..=MAX_OUTLETS)
            .map(|i| Outlet {
                name: format!("cmd{i}"),
                type_name: "DigitalState".into(),
                device: "relay1".into(),
                input: Some("ctrl".into()),
                config: IndexMap::new(),
            })
            .collect();
        let pipeline = pipeline_with_outlets(outlets);
        let errors = validate_pipeline_against_manifest(
            &pipeline,
            &manifest,
            &IndexMap::new(),
            &IndexMap::new(),
            32,
        );
        assert!(
            !errors
                .iter()
                .any(|e| e.message.contains("registry holds at most")),
            "feed-forward outlets must not count against the registry: {errors:?}"
        );
    }

    #[test]
    fn feed_forward_unresolvable_input_is_rejected() {
        let manifest = manifest_with_output_device();
        let mut pipeline = feed_forward_pipeline();
        // Point the outlet at a step id that does not exist.
        pipeline.outlets[0].input = Some("does_not_exist".into());
        let (drivers, steps) = feed_forward_schemas("DigitalState");
        let errors = validate_pipeline_against_manifest(&pipeline, &manifest, &drivers, &steps, 32);
        assert!(
            errors.iter().any(|e| {
                e.message.contains("does not resolve") && e.message.contains("does_not_exist")
            }),
            "got: {errors:?}"
        );
    }

    #[test]
    fn feed_forward_via_transparent_step_resolves_upstream() {
        // outlet input -> `cadence` (type-transparent) -> hysteresis (DigitalState).
        // producer_type must walk through the output-less cadence to the upstream type.
        let manifest = manifest_with_output_device();
        let pipeline = PipelineFile {
            pipeline: PipelineInfo { id: "t".into() },
            sources: vec![Source {
                id: "src".into(),
                device: "bme280".into(),
                config: IndexMap::new(),
            }],
            steps: vec![
                Step {
                    id: "ctrl".into(),
                    op: "hysteresis".into(),
                    input: "src.temperature".into(),
                    config: IndexMap::new(),
                },
                Step {
                    id: "gate".into(),
                    op: "cadence".into(),
                    input: "ctrl".into(),
                    config: IndexMap::new(),
                },
            ],
            taps: vec![],
            outlets: vec![Outlet {
                name: "relay1_cmd".into(),
                type_name: "DigitalState".into(),
                device: "relay1".into(),
                input: Some("gate".into()),
                config: IndexMap::new(),
            }],
        };
        let mut drivers = IndexMap::new();
        drivers.insert("gpio-output".into(), writer_schema("DigitalState"));
        drivers.insert(
            "bme280".into(),
            driver_schema(&[("temperature", "f32")], None),
        );
        let mut steps = IndexMap::new();
        steps.insert("hysteresis".into(), step_schema("f32", "DigitalState"));
        // Type-transparent step: no declared inputs/outputs.
        steps.insert("cadence".into(), DriverSchema::default());
        let errors = validate_pipeline_against_manifest(&pipeline, &manifest, &drivers, &steps, 32);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    }
}
