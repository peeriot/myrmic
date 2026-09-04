//! The generated `spawn_sources(spawner, peripherals)` entry point.

use anyhow::{Result, anyhow};
use indexmap::IndexMap;
use proc_macro2::TokenStream;
use quote::quote;

use crate::ChipBackend;
use crate::descriptor::DriverSchema;
use crate::manifest::{BoardManifest, BusTransport};
use crate::pipeline::PipelineFile;

use super::buses::used_bus_ids;
use super::helpers::{bus_static_ident, owned_outlets_for_source, snake_ident};

#[allow(clippy::too_many_lines)] // codegen: single quote! block, not meaningfully splittable
pub(crate) fn emit_spawn_sources(
    pipeline: &PipelineFile,
    manifest: &BoardManifest,
    driver_schemas: &IndexMap<String, DriverSchema>,
    backend: &dyn ChipBackend,
) -> Result<TokenStream> {
    let used_buses = used_bus_ids(pipeline, manifest)?;

    let mut bus_init_stmts = TokenStream::new();
    for bus_id in &used_buses {
        let static_ident = bus_static_ident(bus_id);
        let bus_field = snake_ident(bus_id);
        let bus_var = snake_ident(&format!("{bus_id}_mutex"));
        let bus_cfg = manifest
            .buses
            .get(bus_id.as_str())
            .ok_or_else(|| anyhow!("bus `{bus_id}` not in manifest"))?;
        let inner = match bus_cfg.transport {
            BusTransport::I2c => backend.i2c_bus_type(),
            BusTransport::Spi => backend.spi_bus_type(),
        };
        let bus_var_ts: TokenStream = quote! { #bus_var };
        let static_ident_ts: TokenStream = quote! { #static_ident };
        let bus_field_ts: TokenStream = quote! { #bus_field };
        bus_init_stmts.extend(backend.emit_bus_init(
            &bus_var_ts,
            &static_ident_ts,
            &bus_field_ts,
            &inner,
        ));
    }

    let mut spawn_stmts = TokenStream::new();
    for source in &pipeline.sources {
        let device_driver = manifest
            .devices
            .iter()
            .find(|d| d.id == source.device)
            .ok_or_else(|| anyhow!("device `{}` not found", source.device))?;
        let task_fn = snake_ident(&format!("{}_task", source.id));
        let bus_var = snake_ident(&format!("{}_mutex", device_driver.bus));
        let bus_cfg = manifest
            .buses
            .get(&device_driver.bus)
            .ok_or_else(|| anyhow!("bus `{}` not in manifest", device_driver.bus))?;
        let bus_var_ts: TokenStream = quote! { #bus_var };
        let spawn_arg = match bus_cfg.transport {
            BusTransport::I2c => backend.emit_bus_device_new(&bus_var_ts),
            BusTransport::Spi => {
                let cs_field = snake_ident(&format!("{}_cs", device_driver.id));
                let cs_field_ts: TokenStream = quote! { #cs_field };
                backend.emit_spi_bus_device_new(&bus_var_ts, &cs_field_ts)
            }
        };
        let schema = driver_schemas
            .get(device_driver.driver.as_str())
            .cloned()
            .unwrap_or_default();
        let mut pin_args = TokenStream::new();
        for pin_name in &schema.requires.optional_pins {
            if device_driver.pins.contains_key(pin_name.as_str()) {
                let field_ident = snake_ident(&format!("{}_{}", device_driver.id, pin_name));
                pin_args.extend(quote! { peripherals.#field_ident, });
            }
        }
        // Feed-forward outlets owned by this source: their output pins follow the
        // source's own pins as task args, matching emit_source_task's param order.
        for outlet in owned_outlets_for_source(&source.id, pipeline) {
            let odevice = manifest
                .devices
                .iter()
                .find(|d| d.id == outlet.device)
                .ok_or_else(|| {
                    anyhow!(
                        "outlet `{}`: device `{}` not found",
                        outlet.name,
                        outlet.device
                    )
                })?;
            let oschema = driver_schemas
                .get(odevice.driver.as_str())
                .cloned()
                .unwrap_or_default();
            for pin_name in &oschema.requires.optional_pins {
                if odevice.pins.contains_key(pin_name.as_str()) {
                    let field_ident = snake_ident(&format!("{}_{}", odevice.id, pin_name));
                    pin_args.extend(quote! { peripherals.#field_ident, });
                }
            }
        }
        // Pass fn(args) without .expect() — the backend's emit_spawn constructs
        // the full spawn statement. Embassy adds the .expect(label) for SpawnToken
        // unwrapping; the tokio backend wraps it as a plain async spawn (no unwrap).
        let task_expr: TokenStream = quote! { #task_fn(#spawn_arg, #pin_args) };
        spawn_stmts.extend(backend.emit_spawn(&task_expr, "failed to spawn source task"));
    }

    // Sink tasks (cell-driven outlets only): each takes its owned output pins
    // from `BoardPeripherals` (emitted as `<device>_<pin>` fields — an `Output`
    // for digital devices, or a configured PWM/LEDC channel for PWM). Pipeline-
    // driven outlets are spawned inline with their source task, not here.
    for outlet in pipeline.outlets.iter().filter(|o| o.input.is_none()) {
        let device = manifest
            .devices
            .iter()
            .find(|d| d.id == outlet.device)
            .ok_or_else(|| {
                anyhow!(
                    "outlet `{}`: device `{}` not found",
                    outlet.name,
                    outlet.device
                )
            })?;
        let task_fn = snake_ident(&format!("{}_sink_task", device.id));
        let schema = driver_schemas
            .get(device.driver.as_str())
            .cloned()
            .unwrap_or_default();
        let mut pin_args = TokenStream::new();
        for pin_name in &schema.requires.optional_pins {
            if device.pins.contains_key(pin_name.as_str()) {
                let field_ident = snake_ident(&format!("{}_{}", device.id, pin_name));
                pin_args.extend(quote! { peripherals.#field_ident, });
            }
        }
        // Same pattern: no .expect() here — embassy's emit_spawn adds it.
        let task_expr: TokenStream = quote! { #task_fn(#pin_args) };
        spawn_stmts.extend(backend.emit_spawn(&task_expr, "failed to spawn sink task"));
    }

    Ok(quote! {
        pub fn spawn_sources(spawner: &Spawner, peripherals: BoardPeripherals) {
            #bus_init_stmts
            #spawn_stmts
        }
    })
}
