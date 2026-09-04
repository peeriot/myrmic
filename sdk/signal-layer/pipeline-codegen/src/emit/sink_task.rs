//! Per-outlet Embassy sink task: construct driver → init → ticker → read outlet
//! → apply on change → (hybrid) read status back and publish feedback taps. The
//! write-side mirror of `source_task.rs`.

use anyhow::{Result, anyhow, bail};
use proc_macro2::{Ident, Span, TokenStream};
use quote::quote;

use crate::ChipBackend;
use crate::descriptor::{DriverSchema, OutputMode, Scope};
use crate::manifest::DeviceEntry;
use crate::pipeline::{Outlet, PipelineFile};

use super::helpers::{
    config_value_tokens, outlet_static_ident, pascal_case, snake_ident, tap_static_ident,
};

/// How often the latest command is (re-)applied / status is polled when no
/// cadence is configured.
const DEFAULT_WRITE_INTERVAL_MS: u64 = 100;

/// The reserved outlet status field naming the error Event tap (`<outlet>.error`).
const OUTLET_ERROR_FIELD: &str = "error";

#[allow(clippy::too_many_lines)] // codegen: single quote! block, not meaningfully splittable
pub(crate) fn emit_sink_task(
    outlet: &Outlet,
    device: &DeviceEntry,
    driver_schema: &DriverSchema,
    pipeline: &PipelineFile,
    backend: &dyn ChipBackend,
) -> Result<TokenStream> {
    let write = driver_schema.writes.as_ref().ok_or_else(|| {
        anyhow!(
            "outlet `{}`: device `{}` driver `{}` has no `writes` block",
            outlet.name,
            device.id,
            device.driver
        )
    })?;

    let task_fn = snake_ident(&format!("{}_sink_task", device.id));
    // Driver crates follow the `<id>-driver` naming convention.
    let drv_crate = snake_ident(&format!("{}-driver", device.driver));
    let drv_type = Ident::new(&pascal_case(&device.driver), Span::call_site());
    let drv_config = Ident::new(
        &format!("{}Config", pascal_case(&device.driver)),
        Span::call_site(),
    );
    let outlet_static = outlet_static_ident(&outlet.name);
    let device_id_str = &device.id;

    // Driver Config fields: Hardware scope from the manifest device, Application
    // scope from the outlet config, descriptor default as fallback.
    let mut config_fields = TokenStream::new();
    for (field_name, field_def) in &driver_schema.config_schema {
        let field_ident = snake_ident(field_name);
        let value = match field_def.scope {
            Scope::Hardware => device
                .hardware
                .get(field_name.as_str())
                .unwrap_or(&field_def.default),
            Scope::Application => outlet
                .config
                .get(field_name.as_str())
                .unwrap_or(&field_def.default),
        };
        let rust_type = field_def
            .rust_type
            .as_deref()
            .ok_or_else(|| anyhow!("driver field `{field_name}` missing `rust_type`"))?;
        let val_tokens = config_value_tokens(value, rust_type, &drv_crate)?;
        config_fields.extend(quote! { #field_ident: #val_tokens, });
    }

    // Write cadence (application-scope).
    let interval_ms = outlet
        .config
        .get("write_interval_ms")
        .and_then(serde_yaml::Value::as_u64)
        .unwrap_or(DEFAULT_WRITE_INTERVAL_MS);
    if interval_ms == 0 {
        bail!(
            "outlet `{}`: write_interval_ms must be >= 1 (got 0); a zero cadence would spin the sink task",
            outlet.name
        );
    }
    let interval_expr = backend.emit_interval(interval_ms);
    let now_millis_expr = backend.emit_now_millis();

    // Pin task params, in the driver's `optional_pins` order. The `out` pin is
    // the driven output (Output for digital, PWM channel for PWM); every other
    // pin is a feedback input (hybrid devices). Types must match the backend's
    // `emit_board_peripherals`.
    let mut pin_params = TokenStream::new();
    let mut pin_args = TokenStream::new();
    for pin_name in &driver_schema.requires.optional_pins {
        if !device.pins.contains_key(pin_name.as_str()) {
            bail!(
                "outlet `{}`: device `{}` must wire the `{pin_name}` pin required by driver `{}`",
                outlet.name,
                device.id,
                device.driver
            );
        }
        let pin_ident = snake_ident(pin_name);
        let pin_ty = if pin_name == "out" {
            match write.mode {
                OutputMode::Digital => backend.gpio_output_type(),
                OutputMode::Pwm => backend.pwm_channel_type(),
            }
        } else {
            backend.gpio_input_type()
        };
        pin_params.extend(quote! { #pin_ident: #pin_ty, });
        pin_args.extend(quote! { #pin_ident, });
    }

    // Feedback taps for this outlet (#1018): status Retained taps
    // (`<outlet>.<field>`) written from a real read-back, and the reserved
    // `<outlet>.error` Event tap.
    let outlet_prefix = format!("{}.", outlet.name);
    let mut status_updates = TokenStream::new();
    for tap in &pipeline.taps {
        if let Some(field) = tap.source.strip_prefix(&outlet_prefix) {
            if field == OUTLET_ERROR_FIELD {
                continue;
            }
            let tap_static = tap_static_ident(&tap.name);
            let field_ident = snake_ident(field);
            status_updates.extend(quote! { #tap_static.update(ts, readings.#field_ident); });
        }
    }
    let error_static = pipeline
        .taps
        .iter()
        .find(|t| t.source == format!("{}.{OUTLET_ERROR_FIELD}", outlet.name))
        .map(|t| tap_static_ident(&t.name));

    let hybrid = !status_updates.is_empty();
    if hybrid && driver_schema.outputs.is_empty() {
        bail!(
            "outlet `{}`: status taps reference device `{}`, but driver `{}` declares no \
             `outputs` (not a hybrid/feedback driver)",
            outlet.name,
            device.id,
            device.driver
        );
    }

    let write_err = error_static.as_ref().map(|e| {
        quote! { #e.emit(signal_layer_types::OutletFault::WriteFailed); }
    });
    let read_err = error_static.as_ref().map(|e| {
        quote! { #e.emit(signal_layer_types::OutletFault::ReadFailed); }
    });

    // Read the device's real status back and publish it. Status comes ONLY from
    // this read — never inferred from the command write (SDS A1 / OUT-09). Poll
    // when the driver can read (has outputs) and something consumes it: status
    // taps, or an error tap that must surface read failures as ReadFailed.
    let poll_status =
        !driver_schema.outputs.is_empty() && (!status_updates.is_empty() || error_static.is_some());
    let readback = if poll_status {
        quote! {
            match driver.read_status() {
                Ok(readings) => { #status_updates }
                Err(_e) => {
                    log::warn!("[{}] outlet status read failed", #device_id_str);
                    #read_err
                }
            }
        }
    } else {
        quote! {}
    };

    let task_attr = backend.emit_task_attribute();

    Ok(quote! {
        #task_attr
        async fn #task_fn(#pin_params) {
            let cfg = #drv_crate::#drv_config { #config_fields };
            let mut driver = #drv_crate::#drv_type::new(&cfg, #pin_args);
            // Establish the safe (off) state; a failure is logged, not fatal.
            if driver.init().is_err() {
                log::error!("[{}] outlet init failed", #device_id_str);
            }
            // Apply the latest command on a fixed cadence, re-applying only when
            // a new command is written (slot timestamp); then poll device status
            // (hybrid drivers only).
            let mut last_ts = None;
            let mut ticker = #interval_expr;
            loop {
                ticker.next().await;
                let now = #now_millis_expr;
                let ts = Timestamp(now);
                if let Some((slot_ts, cmd)) = #outlet_static.read() {
                    if last_ts != Some(slot_ts) {
                        if driver.apply(cmd, now).is_err() {
                            log::error!("[{}] outlet apply failed", #device_id_str);
                            #write_err
                        } else {
                            last_ts = Some(slot_ts);
                        }
                    }
                }
                #readback
            }
        }
    })
}
