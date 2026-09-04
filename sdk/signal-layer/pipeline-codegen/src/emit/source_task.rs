//! Per-source Embassy task: init → ticker → sample → tap writes → inline DSP chain.

use anyhow::Result;
use indexmap::IndexMap;
use proc_macro2::{Ident, Literal, Span, TokenStream};
use quote::quote;

use crate::ChipBackend;
use crate::descriptor::{DriverSchema, OutputMode, Scope};
use crate::manifest::{BoardManifest, BusConfig, BusTransport, DeviceEntry};
use crate::pipeline::{PipelineFile, Source, TapKind};

use super::helpers::{
    config_value_tokens, owned_outlets_for_source, pascal_case, snake_ident, tap_static_ident,
};

const DEFAULT_SAMPLE_INTERVAL_MS: u64 = 1000;

#[allow(clippy::too_many_lines)] // codegen: single quote! block, not meaningfully splittable
#[expect(
    clippy::too_many_arguments,
    reason = "codegen backend threads manifest + all driver schemas for feed-forward outlets"
)]
pub(crate) fn emit_source_task(
    source: &Source,
    source_idx: u8,
    device: &DeviceEntry,
    bus_cfg: &BusConfig,
    driver_schema: &DriverSchema,
    pipeline: &PipelineFile,
    manifest: &BoardManifest,
    driver_schemas: &IndexMap<String, DriverSchema>,
    step_schemas: &IndexMap<String, DriverSchema>,
    backend: &dyn ChipBackend,
) -> Result<TokenStream> {
    let task_fn = snake_ident(&format!("{}_task", source.id));
    // Driver crates follow the `<id>-driver` naming convention (e.g. `bme280-driver`
    // → `bme280_driver`); DSP step crates use the op id directly.
    let drv_crate = snake_ident(&format!("{}-driver", device.driver));
    let drv_type = Ident::new(&pascal_case(&device.driver), Span::call_site());
    let drv_config = Ident::new(
        &format!("{}Config", pascal_case(&device.driver)),
        Span::call_site(),
    );

    let bus_device_ty = match bus_cfg.transport {
        BusTransport::I2c => {
            let inner = backend.i2c_bus_type();
            backend.emit_bus_device_type(&inner)
        }
        BusTransport::Spi => {
            let inner = backend.spi_bus_type();
            let cs = backend.spi_cs_type();
            backend.emit_spi_bus_device_type(&inner, &cs)
        }
    };

    // Driver Config struct fields. Every config_schema entry maps to a Config
    // field except `sample_interval_ms`, which is task-level (not part of the
    // driver's Config struct — used for the ticker below).
    //
    // - `Hardware` scope: value comes from `device.hardware` in the board manifest
    //   (board wiring decides), with the descriptor default as fallback.
    // - `Application` scope: value comes from `source.config` in the pipeline
    //   (the use case decides), with the descriptor default as fallback.
    let mut config_fields = TokenStream::new();
    for (field_name, field_def) in &driver_schema.config_schema {
        if field_name == "sample_interval_ms" {
            continue;
        }
        let field_ident = snake_ident(field_name);
        let value = match field_def.scope {
            Scope::Hardware => device
                .hardware
                .get(field_name.as_str())
                .unwrap_or(&field_def.default),
            Scope::Application => source
                .config
                .get(field_name.as_str())
                .unwrap_or(&field_def.default),
        };
        let rust_type = field_def
            .rust_type
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("driver field `{field_name}` missing `rust_type`"))?;
        let val_tokens = config_value_tokens(value, rust_type, &drv_crate)?;
        config_fields.extend(quote! { #field_ident: #val_tokens, });
    }

    // Sample interval (application-scope, from pipeline config or descriptor default)
    let interval_ms = source
        .config
        .get("sample_interval_ms")
        .and_then(serde_yaml::Value::as_u64)
        .or_else(|| {
            driver_schema
                .config_schema
                .get("sample_interval_ms")
                .and_then(|f| f.default.as_u64())
        })
        .unwrap_or(DEFAULT_SAMPLE_INTERVAL_MS);
    let interval_expr = backend.emit_interval(interval_ms);
    let now_millis_expr = backend.emit_now_millis();

    // Optional pins: all declared by the driver descriptor, plus which ones the
    // manifest wired up. The task function takes one Flex param per wired pin;
    // the `<Driver>Pins` struct contains every declared pin as `Option<Flex>`.
    let optional_pin_names: Vec<&str> = driver_schema
        .requires
        .optional_pins
        .iter()
        .map(String::as_str)
        .collect();
    let wired_pin_names: Vec<&str> = optional_pin_names
        .iter()
        .copied()
        .filter(|name| device.pins.contains_key(*name))
        .collect();

    let mut extra_task_params = TokenStream::new();
    for pin_name in &wired_pin_names {
        let pin_ident = snake_ident(pin_name);
        let flex_ty = backend.gpio_flex_type();
        extra_task_params.extend(quote! { #pin_ident: #flex_ty, });
    }

    // Infallible construction (no bus access). The fallible bring-up happens
    // via `driver.init(&mut bus)` inside the task loop, so a sensor that is
    // absent or failing at boot can be retried instead of killing the task.
    let construct_expr = if wired_pin_names.is_empty() {
        quote! { #drv_crate::#drv_type::new(&cfg) }
    } else {
        let pins_type = Ident::new(
            &format!("{}Pins", pascal_case(&device.driver)),
            Span::call_site(),
        );
        let mut pin_fields = TokenStream::new();
        for pin_name in &optional_pin_names {
            let field_ident = snake_ident(pin_name);
            if wired_pin_names.contains(pin_name) {
                pin_fields.extend(quote! { #field_ident: Some(#field_ident), });
            } else {
                pin_fields.extend(quote! { #field_ident: None, });
            }
        }
        quote! {
            #drv_crate::#drv_type::new_with_pins(
                &cfg,
                #drv_crate::#pins_type { #pin_fields },
            )
        }
    };

    // Direct tap writes (taps whose source is "{source_id}.{field}").
    //
    // `retained_clears` accumulates a `.clear()` for every retained tap this
    // task writes (direct + step-derived, below). When the source leaves the
    // healthy state its retained taps are cleared so consumers read no value
    // rather than a stale one produced before the fault.
    let source_prefix = format!("{}.", source.id);
    let mut tap_writes = TokenStream::new();
    let mut retained_clears = TokenStream::new();
    for tap in &pipeline.taps {
        if let Some(field_name) = tap.source.strip_prefix(&source_prefix) {
            let static_name = tap_static_ident(&tap.name);
            let field_ident = snake_ident(field_name);
            match tap.kind {
                TapKind::Retained => {
                    tap_writes.extend(quote! {
                        #static_name.update(ts, readings.#field_ident);
                    });
                    retained_clears.extend(quote! { #static_name.clear(); });
                }
                TapKind::Event => {
                    tap_writes.extend(quote! {
                        #static_name.emit(readings.#field_ident);
                    });
                }
                TapKind::Batch => {
                    return Err(anyhow::anyhow!(
                        "tap `{}`: batch taps not yet supported by codegen",
                        tap.name
                    ));
                }
            }
        }
    }

    // Processing steps connected to this source, emitted in topological order.
    //
    // A step whose `input` is `"source_id.field"` connects directly to this
    // source's readings. A step whose `input` is another step's id chains off
    // that step's output via `Option::and_then`. We do a fixed-point walk so
    // a chain of arbitrary depth is emitted correctly — steps unreachable from
    // this source are left for the other source's task (or flagged by
    // validation if they are dangling).
    let mut step_state_inits = TokenStream::new();
    let mut dsp_chain = TokenStream::new();

    // ── Feed-forward outlets driven by this source ──────────────────────────
    // Each pipeline-driven outlet whose input roots at this source has its
    // driver constructed and owned here; the command is applied inline once the
    // input value is available (below, in the DSP chain / readings block).
    let owned_outlets = owned_outlets_for_source(&source.id, pipeline);
    let mut outlet_driver_inits = TokenStream::new();
    let mut outlet_applies: Vec<(Ident, String)> = Vec::new();
    let mut outlet_step_inputs: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for outlet in &owned_outlets {
        let input = outlet.input.as_deref().expect("owned outlet has input");
        let odevice = manifest
            .devices
            .iter()
            .find(|d| d.id == outlet.device)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "outlet `{}`: device `{}` not in manifest",
                    outlet.name,
                    outlet.device
                )
            })?;
        let oschema = driver_schemas
            .get(odevice.driver.as_str())
            .cloned()
            .unwrap_or_default();
        let write = oschema.writes.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "outlet `{}`: device `{}` driver `{}` has no `writes` block",
                outlet.name,
                odevice.id,
                odevice.driver
            )
        })?;
        let ocrate = snake_ident(&format!("{}-driver", odevice.driver));
        let otype = Ident::new(&pascal_case(&odevice.driver), Span::call_site());
        let oconfig = Ident::new(
            &format!("{}Config", pascal_case(&odevice.driver)),
            Span::call_site(),
        );
        let driver_var = snake_ident(&format!("{}_driver", odevice.id));
        let cfg_var = snake_ident(&format!("{}_cfg", odevice.id));

        let mut ocfg_fields = TokenStream::new();
        for (field_name, field_def) in &oschema.config_schema {
            let field_ident = snake_ident(field_name);
            let value = match field_def.scope {
                Scope::Hardware => odevice
                    .hardware
                    .get(field_name.as_str())
                    .unwrap_or(&field_def.default),
                Scope::Application => outlet
                    .config
                    .get(field_name.as_str())
                    .unwrap_or(&field_def.default),
            };
            let rust_type = field_def.rust_type.as_deref().ok_or_else(|| {
                anyhow::anyhow!("outlet driver field `{field_name}` missing `rust_type`")
            })?;
            let val_tokens = config_value_tokens(value, rust_type, &ocrate)?;
            ocfg_fields.extend(quote! { #field_ident: #val_tokens, });
        }

        // Output pin(s) become task params with the proper output type
        // (digital `Output` or PWM channel), and are moved into the driver.
        for pin_name in &oschema.requires.optional_pins {
            if !odevice.pins.contains_key(pin_name.as_str()) {
                return Err(anyhow::anyhow!(
                    "outlet `{}`: device `{}` must wire the `{pin_name}` pin",
                    outlet.name,
                    odevice.id
                ));
            }
            let pin_ident = snake_ident(&format!("{}_{}", odevice.id, pin_name));
            let pin_ty = match write.mode {
                OutputMode::Digital => backend.gpio_output_type(),
                OutputMode::Pwm => backend.pwm_channel_type(),
            };
            extra_task_params.extend(quote! { #pin_ident: #pin_ty, });
        }
        let first_pin = oschema.requires.optional_pins.first().ok_or_else(|| {
            anyhow::anyhow!("outlet `{}`: driver declares no output pin", outlet.name)
        })?;
        let pin_arg = snake_ident(&format!("{}_{}", odevice.id, first_pin));
        let odev_id = &odevice.id;
        outlet_driver_inits.extend(quote! {
            let #cfg_var = #ocrate::#oconfig { #ocfg_fields };
            let mut #driver_var = #ocrate::#otype::new(&#cfg_var, #pin_arg);
            if #driver_var.init().is_err() {
                log::error!("[{}] outlet init failed", #odev_id);
            }
        });

        if !input.contains('.') {
            outlet_step_inputs.insert(input);
        }
        outlet_applies.push((driver_var, input.to_string()));
    }

    // Pre-pass: which step ids are consumed as input by another step or fed to a
    // feed-forward outlet? Those need a named Option output variable.
    let consumed_by_downstream: std::collections::HashSet<&str> = pipeline
        .steps
        .iter()
        .filter(|n| !n.input.contains('.'))
        .map(|n| n.input.as_str())
        .chain(outlet_step_inputs.iter().copied())
        .collect();

    // Maps step id → the Ident of its emitted `Option` output var.
    let mut step_out_vars: IndexMap<&str, Ident> = IndexMap::new();

    let mut remaining: Vec<&crate::pipeline::Step> = pipeline.steps.iter().collect();
    loop {
        let before = remaining.len();
        let mut next = Vec::new();

        for step in remaining {
            // Resolve the input expression: source field or a previous step's output var.
            let input_ts = if let Some(field) = step.input.strip_prefix(&source_prefix) {
                let f = snake_ident(field);
                Some(quote! { readings.#f })
            } else if let Some(out_var) = step_out_vars.get(step.input.as_str()) {
                Some(quote! { #out_var })
            } else {
                next.push(step);
                continue;
            };
            let input_ts = input_ts.unwrap();
            let is_source_input = step.input.starts_with(&source_prefix);

            let step_crate = snake_ident(&step.op);
            let step_state_type = Ident::new(
                &format!("{}State", pascal_case(&step.op)),
                Span::call_site(),
            );
            let step_config_type = Ident::new(
                &format!("{}Config", pascal_case(&step.op)),
                Span::call_site(),
            );
            let step_var = snake_ident(&format!("{}_node", step.id));

            let schema = step_schemas
                .get(step.op.as_str())
                .cloned()
                .unwrap_or_default();
            let mut step_cfg_fields = TokenStream::new();
            for (field_name, field_def) in &schema.config_schema {
                let field_ident = snake_ident(field_name);
                let value = step
                    .config
                    .get(field_name.as_str())
                    .unwrap_or(&field_def.default);
                let rust_type = field_def.rust_type.as_deref().ok_or_else(|| {
                    anyhow::anyhow!(
                        "step `{}` field `{field_name}` missing `rust_type`",
                        step.id
                    )
                })?;
                let val_tokens = config_value_tokens(value, rust_type, &step_crate)?;
                step_cfg_fields.extend(quote! { #field_ident: #val_tokens, });
            }

            let config_expr = if step_cfg_fields.is_empty() {
                quote! { #step_crate::#step_config_type }
            } else {
                quote! { #step_crate::#step_config_type { #step_cfg_fields } }
            };
            step_state_inits.extend(quote! {
                let mut #step_var = #step_crate::#step_state_type::new(#config_expr);
            });

            // Taps fed by this step's output (inner variable is `v` in the if-let).
            let mut step_tap_writes = TokenStream::new();
            for tap in &pipeline.taps {
                if tap.source == step.id {
                    let static_name = tap_static_ident(&tap.name);
                    match tap.kind {
                        TapKind::Retained => {
                            step_tap_writes.extend(quote! { #static_name.update(ts, v); });
                            retained_clears.extend(quote! { #static_name.clear(); });
                        }
                        TapKind::Event => {
                            step_tap_writes.extend(quote! { #static_name.emit(v); });
                        }
                        TapKind::Batch => {
                            return Err(anyhow::anyhow!(
                                "tap `{}`: batch taps not yet supported by codegen",
                                tap.name
                            ));
                        }
                    }
                }
            }

            let needs_out_var =
                consumed_by_downstream.contains(step.id.as_str()) || !step_tap_writes.is_empty();

            if needs_out_var {
                let out_var = snake_ident(&format!("{}_out", step.id));
                // Emit the step call, chaining through and_then for step inputs.
                if is_source_input {
                    dsp_chain.extend(quote! {
                        let #out_var = #step_var.step(#input_ts);
                    });
                } else {
                    dsp_chain.extend(quote! {
                        let #out_var = #input_ts.and_then(|inp| #step_var.step(inp));
                    });
                }
                if !step_tap_writes.is_empty() {
                    dsp_chain.extend(quote! {
                        if let Some(v) = #out_var {
                            #step_tap_writes
                        }
                    });
                }
                step_out_vars.insert(step.id.as_str(), out_var);
            } else {
                // No taps, no downstream consumer — discard output.
                if is_source_input {
                    dsp_chain.extend(quote! { let _ = #step_var.step(#input_ts); });
                } else {
                    dsp_chain
                        .extend(quote! { let _ = #input_ts.and_then(|inp| #step_var.step(inp)); });
                }
            }
        }

        remaining = next;
        if remaining.len() == before {
            break; // no progress — remaining steps are unreachable from this source
        }
    }

    // Feed-forward applies, emitted after the DSP walk so step output vars exist.
    for (driver_var, input) in &outlet_applies {
        let driver_name = driver_var.to_string();
        if let Some(field) = input.strip_prefix(&source_prefix) {
            let f = snake_ident(field);
            dsp_chain.extend(quote! {
                if #driver_var.apply(readings.#f, ts.0).is_err() {
                    log::error!("[{}] outlet apply failed", #driver_name);
                }
            });
        } else if let Some(out_var) = step_out_vars.get(input.as_str()) {
            dsp_chain.extend(quote! {
                if let Some(v) = #out_var {
                    if #driver_var.apply(v, ts.0).is_err() {
                        log::error!("[{}] outlet apply failed", #driver_name);
                    }
                }
            });
        } else {
            return Err(anyhow::anyhow!(
                "outlet feed-forward input `{input}` is not reachable from source `{}`",
                source.id
            ));
        }
    }

    let src_idx_lit = Literal::u8_suffixed(source_idx);
    let source_id_str = &source.id;
    let task_attr = backend.emit_task_attribute();

    Ok(quote! {
        #task_attr
        async fn #task_fn(mut bus: #bus_device_ty, #extra_task_params) {
            let cfg = #drv_crate::#drv_config { #config_fields };
            let mut driver = #construct_expr;
            #step_state_inits
            #outlet_driver_inits
            // `health` starts Up (no event = healthy, by convention); `ready`
            // tracks whether the sensor has been brought up via `init`.
            let mut health = DriverHealth::Up;
            let mut ready = false;
            let mut ticker = #interval_expr;
            loop {
                ticker.next().await;
                // Bring-up / recovery: (re-)run init until it succeeds. A failure
                // is non-terminal — the task keeps retrying on the ticker cadence.
                if !ready {
                    match driver.init(&mut bus).await {
                        Ok(()) => ready = true,
                        Err(_e) => {
                            if health != DriverHealth::Down {
                                health = DriverHealth::Down;
                                TAP__SIGNAL_LAYER_HEALTH.emit(HealthEvent {
                                    source: #src_idx_lit,
                                    state: DriverHealth::Down,
                                });
                                log::error!("[{}] init failed — sensor Down", #source_id_str);
                            }
                            continue;
                        }
                    }
                }
                match driver.sample(&mut bus).await {
                    Ok(readings) => {
                        if health != DriverHealth::Up {
                            health = DriverHealth::Up;
                            TAP__SIGNAL_LAYER_HEALTH.emit(HealthEvent {
                                source: #src_idx_lit,
                                state: DriverHealth::Up,
                            });
                            log::info!("[{}] recovered — sensor Up", #source_id_str);
                        }
                        let ts = Timestamp(#now_millis_expr);
                        #tap_writes
                        #dsp_chain
                    }
                    Err(_e) => {
                        // Drop to bring-up mode so the next tick re-runs init().
                        ready = false;
                        if health == DriverHealth::Up {
                            health = DriverHealth::Degraded;
                            // Invalidate retained taps so consumers read no value
                            // rather than a stale one while the sensor is faulted.
                            #retained_clears
                            TAP__SIGNAL_LAYER_HEALTH.emit(HealthEvent {
                                source: #src_idx_lit,
                                state: DriverHealth::Degraded,
                            });
                            log::warn!("[{}] sample error — sensor Degraded", #source_id_str);
                        }
                    }
                }
            }
        }
    })
}
