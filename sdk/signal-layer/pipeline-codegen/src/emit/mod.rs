//! Code generation: board manifest + pipeline → formatted Rust source.

use anyhow::{Result, anyhow};
use indexmap::IndexMap;
use proc_macro2::TokenStream;

use crate::ChipBackend;
use crate::descriptor::DriverSchema;
use crate::manifest::BoardManifest;
use crate::pipeline::PipelineFile;

mod buses;
mod helpers;
mod imports;
mod outlets;
mod sink_task;
mod source_task;
mod spawn;
mod taps;

/// Generate a formatted Rust source file from a board manifest + pipeline.
///
/// `driver_schemas` — keyed by driver id (e.g. `"bme280"`).
/// `step_schemas`   — keyed by step op id (e.g. `"moving-average"`).
pub fn generate(
    manifest: &BoardManifest,
    pipeline: &PipelineFile,
    driver_schemas: &IndexMap<String, DriverSchema>,
    step_schemas: &IndexMap<String, DriverSchema>,
    backend: &dyn ChipBackend,
) -> Result<String> {
    let tokens = generate_tokens(manifest, pipeline, driver_schemas, step_schemas, backend)?;
    let file: syn::File =
        syn::parse2(tokens).map_err(|e| anyhow!("generated code failed to parse: {e}"))?;
    Ok(prettyplease::unparse(&file))
}

// source_idx is cast to u8 for the health-event source id; pipelines are validated to have
// at most MAX_TAPS−1 sources (≤15), so the index never exceeds u8::MAX.
#[allow(clippy::cast_possible_truncation)]
fn generate_tokens(
    manifest: &BoardManifest,
    pipeline: &PipelineFile,
    driver_schemas: &IndexMap<String, DriverSchema>,
    step_schemas: &IndexMap<String, DriverSchema>,
    backend: &dyn ChipBackend,
) -> Result<TokenStream> {
    let mut ts = TokenStream::new();

    ts.extend(imports::emit_common_imports(backend));
    ts.extend(taps::emit_tap_statics(pipeline)?);
    ts.extend(outlets::emit_outlet_statics(pipeline));
    ts.extend(backend.emit_board_peripherals(manifest, driver_schemas));
    ts.extend(buses::emit_bus_statics(pipeline, manifest, backend)?);

    for (source_idx, source) in pipeline.sources.iter().enumerate() {
        let device = manifest
            .devices
            .iter()
            .find(|d| d.id == source.device)
            .ok_or_else(|| {
                anyhow!(
                    "source `{}`: device `{}` not in manifest",
                    source.id,
                    source.device
                )
            })?;
        let schema = driver_schemas
            .get(device.driver.as_str())
            .cloned()
            .unwrap_or_default();
        let bus_cfg = manifest.buses.get(&device.bus).ok_or_else(|| {
            anyhow!(
                "source `{}`: bus `{}` not in manifest",
                source.id,
                device.bus
            )
        })?;
        ts.extend(source_task::emit_source_task(
            source,
            source_idx as u8,
            device,
            bus_cfg,
            &schema,
            pipeline,
            manifest,
            driver_schemas,
            step_schemas,
            backend,
        )?);
    }

    // Only cell-driven outlets get a dedicated sink task; pipeline-driven
    // (feed-forward) outlets are applied inline in their source task.
    for outlet in pipeline.outlets.iter().filter(|o| o.input.is_none()) {
        let device = manifest
            .devices
            .iter()
            .find(|d| d.id == outlet.device)
            .ok_or_else(|| {
                anyhow!(
                    "outlet `{}`: device `{}` not in manifest",
                    outlet.name,
                    outlet.device
                )
            })?;
        let schema = driver_schemas
            .get(device.driver.as_str())
            .cloned()
            .unwrap_or_default();
        ts.extend(sink_task::emit_sink_task(
            outlet, device, &schema, pipeline, backend,
        )?);
    }

    ts.extend(spawn::emit_spawn_sources(
        pipeline,
        manifest,
        driver_schemas,
        backend,
    )?);
    ts.extend(taps::emit_register_taps(pipeline)?);
    ts.extend(taps::emit_setup_tap_registry(backend));
    ts.extend(outlets::emit_register_outlets(pipeline));
    ts.extend(outlets::emit_setup_outlet_registry(backend));
    ts.extend(backend.emit_pipeline_pins_macro(manifest));
    Ok(ts)
}

#[cfg(test)]
mod tests {
    use super::generate;
    use super::helpers::pascal_case;
    use crate::ChipBackend;
    use crate::descriptor::{ConfigField, DriverSchema, Scope};
    use crate::manifest::{BoardManifest, parse_manifest};
    use crate::pipeline::{
        Outlet, PipelineFile, PipelineInfo, Source, Tap, TapKind, TapStreamKind,
    };
    use indexmap::IndexMap;
    use proc_macro2::TokenStream;
    use quote::quote;

    struct MockBackend;
    impl ChipBackend for MockBackend {
        fn emit_imports(&self) -> TokenStream {
            quote!()
        }
        fn emit_board_peripherals(
            &self,
            _manifest: &BoardManifest,
            _driver_schemas: &IndexMap<String, DriverSchema>,
        ) -> TokenStream {
            quote!(
                pub struct BoardPeripherals {
                    pub i2c0: MockI2c,
                }
            )
        }
        fn i2c_bus_type(&self) -> TokenStream {
            quote!(MockI2c)
        }
        fn spi_bus_type(&self) -> TokenStream {
            panic!("MockBackend has no SPI")
        }
        fn spi_cs_type(&self) -> TokenStream {
            panic!("MockBackend has no SPI")
        }
        fn gpio_flex_type(&self) -> TokenStream {
            quote!(MockFlex)
        }
        fn gpio_output_type(&self) -> TokenStream {
            quote!(MockOutput)
        }
        fn pwm_channel_type(&self) -> TokenStream {
            quote!(MockPwm)
        }
        fn gpio_input_type(&self) -> TokenStream {
            quote!(MockInput)
        }
        fn emit_pipeline_pins_macro(&self, _manifest: &BoardManifest) -> TokenStream {
            // Tests don't exercise pipeline_pins!; emit an empty stub so the
            // generated output still parses.
            quote!()
        }
    }

    fn test_manifest() -> BoardManifest {
        parse_manifest(
            r"
id: test-board
chip: test
buses:
  i2c0:
    transport: i2c
    pins:
      scl: 10
      sda: 11
    freq_khz: 400
gpios:
  general_purpose: [0, 1, 2, 3, 4, 5]
devices:
  - id: bme280
    driver: bme280
    bus: i2c0
    hardware:
      i2c_addr: 0x76
  - id: veml7700
    driver: veml7700
    bus: i2c0
",
        )
        .unwrap()
    }

    fn bme280_schema() -> DriverSchema {
        let mut cs = IndexMap::new();
        cs.insert(
            "i2c_addr".into(),
            ConfigField {
                scope: Scope::Hardware,
                rust_type: Some("u8".into()),
                default: serde_yaml::Value::Number(0x76.into()),
            },
        );
        cs.insert(
            "sample_interval_ms".into(),
            ConfigField {
                scope: Scope::Application,
                rust_type: Some("u64".into()),
                default: serde_yaml::Value::Number(1000.into()),
            },
        );
        DriverSchema {
            config_schema: cs,
            ..Default::default()
        }
    }

    fn two_tap_pipeline() -> PipelineFile {
        PipelineFile {
            pipeline: PipelineInfo { id: "test".into() },
            sources: vec![Source {
                id: "bme280".into(),
                device: "bme280".into(),
                config: IndexMap::new(),
            }],
            steps: vec![],
            taps: vec![
                Tap {
                    name: "temperature".into(),
                    kind: TapKind::Retained,
                    type_name: "f32".into(),
                    source: "bme280.temperature".into(),
                    stream_kind: TapStreamKind::Metric,
                },
                Tap {
                    name: "humidity".into(),
                    kind: TapKind::Retained,
                    type_name: "f32".into(),
                    source: "bme280.humidity".into(),
                    stream_kind: TapStreamKind::Metric,
                },
            ],
            outlets: vec![],
        }
    }

    #[test]
    fn tap_statics_are_emitted() {
        let pipeline = two_tap_pipeline();
        let output = generate(
            &test_manifest(),
            &pipeline,
            &{
                let mut m = IndexMap::new();
                m.insert("bme280".into(), bme280_schema());
                m
            },
            &IndexMap::new(),
            &MockBackend,
        )
        .unwrap();

        assert!(
            output.contains("TAP_TEMPERATURE"),
            "missing TAP_TEMPERATURE in:\n{output}"
        );
        assert!(
            output.contains("TAP_HUMIDITY"),
            "missing TAP_HUMIDITY in:\n{output}"
        );
        assert!(
            output.contains("RetainedSlot"),
            "missing RetainedSlot in:\n{output}"
        );
    }

    #[test]
    fn source_task_is_emitted() {
        let pipeline = two_tap_pipeline();
        let mut driver_schemas = IndexMap::new();
        driver_schemas.insert("bme280".into(), bme280_schema());

        let output = generate(
            &test_manifest(),
            &pipeline,
            &driver_schemas,
            &IndexMap::new(),
            &MockBackend,
        )
        .unwrap();

        assert!(
            output.contains("bme280_task"),
            "missing bme280_task in:\n{output}"
        );
        assert!(
            output.contains("Bme280Config"),
            "missing Bme280Config in:\n{output}"
        );
        assert!(
            output.contains("Bme280"),
            "missing Bme280 driver type in:\n{output}"
        );
        assert!(
            output.contains("i2c_addr"),
            "missing i2c_addr in:\n{output}"
        );
    }

    #[test]
    fn dsp_step_is_emitted_inline() {
        use crate::pipeline::Step;

        let mut pipeline = two_tap_pipeline();
        pipeline.steps.push(Step {
            id: "avg_temp".into(),
            op: "moving-average".into(),
            input: "bme280.temperature".into(),
            config: {
                let mut m = IndexMap::new();
                m.insert("window".into(), serde_yaml::Value::Number(4.into()));
                m
            },
        });
        pipeline.taps.push(Tap {
            name: "avg_temperature".into(),
            kind: TapKind::Retained,
            type_name: "f32".into(),
            source: "avg_temp".into(),
            stream_kind: TapStreamKind::Metric,
        });

        let mut driver_schemas = IndexMap::new();
        driver_schemas.insert("bme280".into(), bme280_schema());

        let mut step_schemas = IndexMap::new();
        let mut cs = IndexMap::new();
        cs.insert(
            "window".into(),
            ConfigField {
                scope: Scope::Application,
                rust_type: Some("usize".into()),
                default: serde_yaml::Value::Number(8.into()),
            },
        );
        step_schemas.insert(
            "moving-average".into(),
            DriverSchema {
                config_schema: cs,
                ..Default::default()
            },
        );

        let output = generate(
            &test_manifest(),
            &pipeline,
            &driver_schemas,
            &step_schemas,
            &MockBackend,
        )
        .unwrap();

        assert!(
            output.contains("avg_temp_node"),
            "missing avg_temp_node in:\n{output}"
        );
        assert!(
            output.contains("MovingAverageState"),
            "missing MovingAverageState in:\n{output}"
        );
        assert!(
            output.contains("MovingAverageConfig"),
            "missing MovingAverageConfig in:\n{output}"
        );
        assert!(
            output.contains("TAP_AVG_TEMPERATURE"),
            "missing TAP_AVG_TEMPERATURE in:\n{output}"
        );
    }

    #[test]
    fn step_to_step_chain_is_emitted() {
        use crate::pipeline::Step;

        // Pipeline: bme280.temperature → avg_temp (moving-average) → threshold (threshold)
        // The second step's input is the first step's id, not a source field.
        let mut pipeline = two_tap_pipeline();
        pipeline.steps.push(Step {
            id: "avg_temp".into(),
            op: "moving-average".into(),
            input: "bme280.temperature".into(),
            config: IndexMap::new(),
        });
        pipeline.steps.push(Step {
            id: "threshold".into(),
            op: "threshold".into(),
            input: "avg_temp".into(), // step-to-step reference
            config: IndexMap::new(),
        });
        pipeline.taps.push(Tap {
            name: "avg_temperature".into(),
            kind: TapKind::Retained,
            type_name: "f32".into(),
            source: "avg_temp".into(),
            stream_kind: TapStreamKind::Metric,
        });
        pipeline.taps.push(Tap {
            name: "threshold_out".into(),
            kind: TapKind::Retained,
            type_name: "f32".into(),
            source: "threshold".into(),
            stream_kind: TapStreamKind::Metric,
        });

        let mut driver_schemas = IndexMap::new();
        driver_schemas.insert("bme280".into(), bme280_schema());

        let moving_avg_schema = DriverSchema {
            config_schema: IndexMap::new(),
            ..Default::default()
        };
        let threshold_schema = DriverSchema {
            config_schema: IndexMap::new(),
            ..Default::default()
        };
        let mut step_schemas = IndexMap::new();
        step_schemas.insert("moving-average".into(), moving_avg_schema);
        step_schemas.insert("threshold".into(), threshold_schema);

        let output = generate(
            &test_manifest(),
            &pipeline,
            &driver_schemas,
            &step_schemas,
            &MockBackend,
        )
        .unwrap();

        // Both steps must appear.
        assert!(
            output.contains("avg_temp_node"),
            "missing avg_temp_node in:\n{output}"
        );
        assert!(
            output.contains("threshold_node"),
            "missing threshold_node in:\n{output}"
        );

        // The second step must chain off the first via and_then, not a direct step call.
        assert!(
            output.contains("and_then"),
            "expected and_then chaining for step-to-step input in:\n{output}"
        );

        // Both taps must be wired up.
        assert!(
            output.contains("TAP_AVG_TEMPERATURE"),
            "missing TAP_AVG_TEMPERATURE in:\n{output}"
        );
        assert!(
            output.contains("TAP_THRESHOLD_OUT"),
            "missing TAP_THRESHOLD_OUT in:\n{output}"
        );
    }

    #[test]
    fn empty_config_schema_emits_unit_constructor() {
        use crate::pipeline::Step;

        let mut pipeline = two_tap_pipeline();
        pipeline.steps.push(Step {
            id: "avg_temp".into(),
            op: "moving-average".into(),
            input: "bme280.temperature".into(),
            config: IndexMap::new(),
        });
        pipeline.taps.push(Tap {
            name: "avg_temperature".into(),
            kind: TapKind::Retained,
            type_name: "f32".into(),
            source: "avg_temp".into(),
            stream_kind: TapStreamKind::Metric,
        });

        let mut driver_schemas = IndexMap::new();
        driver_schemas.insert("bme280".into(), bme280_schema());
        let mut step_schemas = IndexMap::new();
        step_schemas.insert(
            "moving-average".into(),
            DriverSchema {
                config_schema: IndexMap::new(),
                ..Default::default()
            },
        );

        let output = generate(
            &test_manifest(),
            &pipeline,
            &driver_schemas,
            &step_schemas,
            &MockBackend,
        )
        .unwrap();

        // Unit structs must be constructed without braces.
        assert!(
            !output.contains("MovingAverageConfig {"),
            "unit config must not use braced constructor in:\n{output}"
        );
        assert!(
            output.contains("MovingAverageConfig"),
            "missing MovingAverageConfig in:\n{output}"
        );
    }

    fn manifest_with_pin_device(pin_entries: &str) -> BoardManifest {
        parse_manifest(&format!(
            r"
id: test-board
chip: test
buses:
  i2c0:
    transport: i2c
    pins: {{ scl: 10, sda: 11 }}
    freq_khz: 400
gpios:
  general_purpose: [0, 1, 2, 3, 4, 5]
devices:
  - id: ccs811
    driver: ccs811
    bus: i2c0
{pin_entries}
"
        ))
        .unwrap()
    }

    fn ccs811_schema(optional_pins: &[&str]) -> DriverSchema {
        use crate::descriptor::Requires;
        DriverSchema {
            requires: Requires {
                buses: vec![],
                optional_pins: optional_pins
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect(),
            },
            ..Default::default()
        }
    }

    fn ccs811_pipeline() -> PipelineFile {
        PipelineFile {
            pipeline: PipelineInfo { id: "test".into() },
            sources: vec![Source {
                id: "aq".into(),
                device: "ccs811".into(),
                config: IndexMap::new(),
            }],
            steps: vec![],
            taps: vec![],
            outlets: vec![],
        }
    }

    #[test]
    fn wired_optional_pin_emits_pins_struct_with_some() {
        // Descriptor declares `nint`, manifest wires it → codegen must emit
        // `new_with_pins(...)` with a `Ccs811Pins { nint: Some(nint) }` literal.
        let manifest = manifest_with_pin_device("    pins:\n      nint: 4");
        let mut schemas = IndexMap::new();
        schemas.insert("ccs811".into(), ccs811_schema(&["nint"]));

        let output = generate(
            &manifest,
            &ccs811_pipeline(),
            &schemas,
            &IndexMap::new(),
            &MockBackend,
        )
        .unwrap();

        assert!(
            output.contains("new_with_pins"),
            "expected new_with_pins call in:\n{output}"
        );
        assert!(
            output.contains("Ccs811Pins"),
            "missing Ccs811Pins struct literal in:\n{output}"
        );
        assert!(
            output.contains("nint: Some(nint)"),
            "wired pin must be Some(...) in:\n{output}"
        );
        // The task function must take the wired pin as a Flex param.
        assert!(
            output.contains("nint: MockFlex"),
            "task fn must take wired pin parameter in:\n{output}"
        );
    }

    #[test]
    fn declared_but_unwired_pin_emits_plain_new() {
        // Descriptor declares `nint`, manifest does NOT wire it → codegen
        // must call plain `new(...)` and skip the Pins struct entirely.
        let manifest = manifest_with_pin_device("");
        let mut schemas = IndexMap::new();
        schemas.insert("ccs811".into(), ccs811_schema(&["nint"]));

        let output = generate(
            &manifest,
            &ccs811_pipeline(),
            &schemas,
            &IndexMap::new(),
            &MockBackend,
        )
        .unwrap();

        assert!(
            !output.contains("new_with_pins"),
            "must NOT call new_with_pins when no pins wired:\n{output}"
        );
        assert!(
            !output.contains("Ccs811Pins"),
            "must NOT emit Pins struct when no pins wired:\n{output}"
        );
        assert!(
            output.contains("Ccs811::new"),
            "must call plain new() in:\n{output}"
        );
    }

    #[test]
    fn partially_wired_pins_emit_some_and_none() {
        // Descriptor declares `nint` and `reset`, manifest wires only `nint` →
        // the Pins struct must list `nint: Some(...)` and `reset: None`.
        let manifest = manifest_with_pin_device("    pins:\n      nint: 4");
        let mut schemas = IndexMap::new();
        schemas.insert("ccs811".into(), ccs811_schema(&["nint", "reset"]));

        let output = generate(
            &manifest,
            &ccs811_pipeline(),
            &schemas,
            &IndexMap::new(),
            &MockBackend,
        )
        .unwrap();

        assert!(
            output.contains("nint: Some(nint)"),
            "wired pin must be Some(...) in:\n{output}"
        );
        assert!(
            output.contains("reset: None"),
            "unwired declared pin must be None in:\n{output}"
        );
        // The task fn must only take the wired pin as a parameter.
        assert!(
            output.contains("nint: MockFlex"),
            "task fn must take wired pin in:\n{output}"
        );
        assert!(
            !output.contains("reset: MockFlex"),
            "task fn must NOT take unwired pin in:\n{output}"
        );
    }

    #[test]
    fn pascal_case_converts_correctly() {
        assert_eq!(pascal_case("bme280"), "Bme280");
        assert_eq!(pascal_case("wsen-itds"), "WsenItds");
        assert_eq!(pascal_case("moving-average"), "MovingAverage");
        assert_eq!(pascal_case("threshold_trigger"), "ThresholdTrigger");
        assert_eq!(pascal_case("max-value"), "MaxValue");
    }

    #[test]
    fn generate_output_is_byte_stable_across_runs() {
        let manifest = test_manifest();
        let pipeline = two_tap_pipeline();
        let mut schemas = IndexMap::new();
        schemas.insert("bme280".into(), bme280_schema());

        let a = generate(
            &manifest,
            &pipeline,
            &schemas,
            &IndexMap::new(),
            &MockBackend,
        )
        .unwrap();
        let b = generate(
            &manifest,
            &pipeline,
            &schemas,
            &IndexMap::new(),
            &MockBackend,
        )
        .unwrap();
        assert_eq!(
            a, b,
            "codegen output must be byte-stable across repeated calls"
        );
    }

    #[test]
    fn health_tap_auto_registered_when_sources_present() {
        let pipeline = two_tap_pipeline();
        let mut driver_schemas = IndexMap::new();
        driver_schemas.insert("bme280".into(), bme280_schema());

        let output = generate(
            &test_manifest(),
            &pipeline,
            &driver_schemas,
            &IndexMap::new(),
            &MockBackend,
        )
        .unwrap();

        assert!(
            output.contains("TAP__SIGNAL_LAYER_HEALTH"),
            "missing health tap static in:\n{output}"
        );
        assert!(
            output.contains("EventSlot"),
            "health tap must be an EventSlot in:\n{output}"
        );
        assert!(
            output.contains("HealthEvent"),
            "missing HealthEvent type in:\n{output}"
        );
    }

    #[test]
    fn health_tap_not_registered_when_no_sources() {
        let pipeline = PipelineFile {
            pipeline: PipelineInfo { id: "empty".into() },
            sources: vec![],
            steps: vec![],
            taps: vec![],
            outlets: vec![],
        };
        // Provide a dummy manifest with a device so generation doesn't fail on
        // bus lookups — but the pipeline has no sources.
        let output = generate(
            &test_manifest(),
            &pipeline,
            &IndexMap::new(),
            &IndexMap::new(),
            &MockBackend,
        )
        .unwrap();

        assert!(
            !output.contains("TAP__SIGNAL_LAYER_HEALTH"),
            "health tap must not appear when there are no sources:\n{output}"
        );
    }

    #[test]
    fn health_state_machine_is_emitted_in_task() {
        let pipeline = two_tap_pipeline();
        let mut driver_schemas = IndexMap::new();
        driver_schemas.insert("bme280".into(), bme280_schema());

        let output = generate(
            &test_manifest(),
            &pipeline,
            &driver_schemas,
            &IndexMap::new(),
            &MockBackend,
        )
        .unwrap();

        // Infallible construction + in-loop bring-up (not a fallible init that
        // returns Self and kills the task on failure).
        assert!(
            output.contains("Bme280::new"),
            "missing infallible new() construction:\n{output}"
        );
        assert!(
            output.contains("let mut ready = false"),
            "missing bring-up `ready` flag:\n{output}"
        );
        assert!(
            output.contains("driver.init(&mut bus)"),
            "missing in-loop init() bring-up call:\n{output}"
        );
        // Init failure path — emits Down but is NON-terminal: retries via
        // `continue`, never `return`.
        assert!(
            output.contains("DriverHealth::Down"),
            "missing Down emission on init failure:\n{output}"
        );
        assert!(
            output.contains("continue"),
            "init failure must retry via continue, not exit:\n{output}"
        );
        assert!(
            !output.contains("return;"),
            "init failure must not early-return / exit the task:\n{output}"
        );
        // Sample failure path — first failure → Degraded
        assert!(
            output.contains("DriverHealth::Degraded"),
            "missing Degraded emission on sample error:\n{output}"
        );
        // Degrading must invalidate this source's retained taps.
        assert!(
            output.contains(".clear()"),
            "missing retained-tap clear() on degradation:\n{output}"
        );
        // Recovery path
        assert!(
            output.contains("DriverHealth::Up"),
            "missing Up emission on recovery:\n{output}"
        );
        // Transition guards — must only emit on state change
        assert!(
            output.contains("health != DriverHealth::Up"),
            "missing Up transition guard:\n{output}"
        );
        assert!(
            output.contains("health == DriverHealth::Up"),
            "missing Degraded transition guard:\n{output}"
        );
        assert!(
            output.contains("health != DriverHealth::Down"),
            "missing Down transition guard:\n{output}"
        );
    }

    #[test]
    fn enum_config_field_emits_qualified_path() {
        // A config field with a non-primitive rust_type (enum) and a variant-name
        // string value must emit a qualified path: <crate>::<Type>::<Variant>.
        let manifest = test_manifest();

        // Build a bme280 schema where osrs_t is an enum type.
        let mut cs = IndexMap::new();
        cs.insert(
            "i2c_addr".into(),
            ConfigField {
                scope: Scope::Hardware,
                rust_type: Some("u8".into()),
                default: serde_yaml::Value::Number(0x76.into()),
            },
        );
        cs.insert(
            "osrs_t".into(),
            ConfigField {
                scope: Scope::Application,
                rust_type: Some("Oversampling".into()),
                default: serde_yaml::Value::String("X1".into()),
            },
        );
        cs.insert(
            "sample_interval_ms".into(),
            ConfigField {
                scope: Scope::Application,
                rust_type: Some("u64".into()),
                default: serde_yaml::Value::Number(1000.into()),
            },
        );
        let schema = DriverSchema {
            config_schema: cs,
            ..Default::default()
        };

        // Pipeline overrides osrs_t with X4.
        let pipeline = PipelineFile {
            pipeline: PipelineInfo { id: "test".into() },
            sources: vec![Source {
                id: "bme280".into(),
                device: "bme280".into(),
                config: {
                    let mut m = IndexMap::new();
                    m.insert("osrs_t".into(), serde_yaml::Value::String("X4".into()));
                    m
                },
            }],
            steps: vec![],
            taps: vec![],
            outlets: vec![],
        };

        let output = generate(
            &manifest,
            &pipeline,
            &{
                let mut m = IndexMap::new();
                m.insert("bme280".into(), schema);
                m
            },
            &IndexMap::new(),
            &MockBackend,
        )
        .unwrap();

        // Must contain the qualified enum path.
        assert!(
            output.contains("bme280_driver :: Oversampling :: X4")
                || output.contains("bme280_driver::Oversampling::X4"),
            "expected qualified enum path in:\n{output}"
        );
    }

    fn gpio_output_schema() -> DriverSchema {
        use crate::descriptor::{DriverWrite, OutputMode, Requires};
        let mut cs = IndexMap::new();
        cs.insert(
            "active_low".into(),
            ConfigField {
                scope: Scope::Hardware,
                rust_type: Some("bool".into()),
                default: serde_yaml::Value::Bool(false),
            },
        );
        DriverSchema {
            config_schema: cs,
            requires: Requires {
                buses: vec![],
                optional_pins: vec!["out".into()],
            },
            writes: Some(DriverWrite {
                command_type: "DigitalState".into(),
                mode: OutputMode::Digital,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn outlet_sink_task_and_registry_are_emitted() {
        let manifest = parse_manifest(
            r"
id: test-board
chip: test
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
    pins: { out: 5 }
",
        )
        .unwrap();
        let pipeline = PipelineFile {
            pipeline: PipelineInfo { id: "test".into() },
            sources: vec![],
            steps: vec![],
            taps: vec![],
            outlets: vec![Outlet {
                name: "relay1_cmd".into(),
                type_name: "DigitalState".into(),
                device: "relay1".into(),
                input: None,
                config: IndexMap::new(),
            }],
        };
        let mut schemas = IndexMap::new();
        schemas.insert("gpio-output".into(), gpio_output_schema());
        let output = generate(
            &manifest,
            &pipeline,
            &schemas,
            &IndexMap::new(),
            &MockBackend,
        )
        .unwrap();

        assert!(
            output.contains("static OUTLET_RELAY1_CMD"),
            "outlet static missing:\n{output}"
        );
        assert!(
            output.contains("DigitalState"),
            "command type missing:\n{output}"
        );
        assert!(
            output.contains("fn relay1_sink_task"),
            "sink task missing:\n{output}"
        );
        assert!(
            output.contains("gpio_output_driver :: GpioOutput")
                || output.contains("gpio_output_driver::GpioOutput"),
            "driver construction missing:\n{output}"
        );
        assert!(
            output.contains("fn register_outlets"),
            "register_outlets missing:\n{output}"
        );
        assert!(
            output.contains("fn setup_outlet_registry"),
            "setup_outlet_registry missing:\n{output}"
        );
        assert!(
            output.contains("init_outlet_registry"),
            "runtime init missing:\n{output}"
        );
    }

    #[test]
    fn feed_forward_outlet_applies_inline_no_sink_task() {
        let manifest = parse_manifest(
            r"
id: test-board
chip: test
buses:
  i2c0:
    transport: i2c
    pins: { scl: 10, sda: 11 }
    freq_khz: 400
gpios:
  general_purpose: [0, 1, 2, 3, 4, 5]
devices:
  - id: bme280
    driver: bme280
    bus: i2c0
  - id: relay1
    driver: gpio-output
    pins: { out: 2 }
",
        )
        .unwrap();
        let pipeline = PipelineFile {
            pipeline: PipelineInfo { id: "test".into() },
            sources: vec![Source {
                id: "src".into(),
                device: "bme280".into(),
                config: IndexMap::new(),
            }],
            steps: vec![crate::pipeline::Step {
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
        };
        let mut schemas = IndexMap::new();
        schemas.insert("bme280".into(), bme280_schema());
        schemas.insert("gpio-output".into(), gpio_output_schema());
        let mut step_schemas = IndexMap::new();
        step_schemas.insert("hysteresis".into(), step_schema_out("DigitalState"));
        let output = generate(&manifest, &pipeline, &schemas, &step_schemas, &MockBackend).unwrap();

        // Applied inline in the source task, via the outlet's device driver.
        assert!(
            output.contains("relay1_driver") && output.contains(".apply("),
            "inline apply missing:\n{output}"
        );
        // No dedicated sink task and no cell-facing registration for a feed-forward outlet.
        assert!(
            !output.contains("relay1_sink_task"),
            "feed-forward outlet must not get a sink task:\n{output}"
        );
        assert!(
            !output.contains("OUTLET_RELAY1_CMD"),
            "feed-forward outlet must not get a slot/registration:\n{output}"
        );
    }

    fn step_schema_out(output_ty: &str) -> DriverSchema {
        use crate::descriptor::{DriverInput, DriverOutput};
        DriverSchema {
            inputs: vec![DriverInput {
                name: "value".into(),
                type_name: "f32".into(),
            }],
            outputs: vec![DriverOutput {
                name: "cmd".into(),
                type_name: output_ty.into(),
                unit: String::new(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn hybrid_outlet_reads_status_and_publishes_feedback_taps() {
        use crate::descriptor::{DriverOutput, DriverWrite, OutputMode, Requires};
        let manifest = parse_manifest(
            r"
id: test-board
chip: test
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
    pins: { out: 2, feedback: 3 }
",
        )
        .unwrap();
        let pipeline = PipelineFile {
            pipeline: PipelineInfo { id: "test".into() },
            sources: vec![],
            steps: vec![],
            taps: vec![
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
            ],
            outlets: vec![Outlet {
                name: "heat_relay".into(),
                type_name: "DigitalState".into(),
                device: "relay1".into(),
                input: None,
                config: IndexMap::new(),
            }],
        };
        let mut schemas = IndexMap::new();
        schemas.insert(
            "gpio-output-feedback".into(),
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
            },
        );
        let output = generate(
            &manifest,
            &pipeline,
            &schemas,
            &IndexMap::new(),
            &MockBackend,
        )
        .unwrap();

        assert!(
            output.contains("read_status"),
            "status read-back missing:\n{output}"
        );
        assert!(
            output.contains("TAP_RELAY_CONTACT") && output.contains("readings.contact"),
            "status tap write missing:\n{output}"
        );
        assert!(
            output.contains("OutletFault :: WriteFailed")
                || output.contains("OutletFault::WriteFailed"),
            "error event emit missing:\n{output}"
        );
        // Feedback pin typed as an input (MockInput), out pin as output.
        assert!(
            output.contains("MockInput"),
            "feedback pin should be a gpio input:\n{output}"
        );
    }
}
