//! Shared-bus statics and the set of buses a pipeline uses.

use anyhow::{Result, anyhow};
use proc_macro2::TokenStream;

use crate::ChipBackend;
use crate::manifest::{BoardManifest, BusTransport};
use crate::pipeline::PipelineFile;

use super::helpers::bus_static_ident;

pub(crate) fn emit_bus_statics(
    pipeline: &PipelineFile,
    manifest: &BoardManifest,
    backend: &dyn ChipBackend,
) -> Result<TokenStream> {
    let used_buses = used_bus_ids(pipeline, manifest)?;
    let mut ts = TokenStream::new();
    for bus_id in &used_buses {
        let bus_cfg = manifest
            .buses
            .get(bus_id.as_str())
            .ok_or_else(|| anyhow!("bus `{bus_id}` not in manifest"))?;
        match bus_cfg.transport {
            BusTransport::I2c => {
                let inner = backend.i2c_bus_type();
                let static_ident = bus_static_ident(bus_id);
                let static_ident_ts: TokenStream = quote::quote! { #static_ident };
                ts.extend(backend.emit_bus_static(&static_ident_ts, &inner));
            }
            BusTransport::Spi => {
                let inner = backend.spi_bus_type();
                let static_ident = bus_static_ident(bus_id);
                let static_ident_ts: TokenStream = quote::quote! { #static_ident };
                ts.extend(backend.emit_bus_static(&static_ident_ts, &inner));
            }
        }
    }
    Ok(ts)
}

pub(crate) fn used_bus_ids(
    pipeline: &PipelineFile,
    manifest: &BoardManifest,
) -> Result<Vec<String>> {
    let mut seen = indexmap::IndexSet::new();
    for source in &pipeline.sources {
        let device = manifest
            .devices
            .iter()
            .find(|d| d.id == source.device)
            .ok_or_else(|| anyhow!("device `{}` not in manifest", source.device))?;
        seen.insert(device.bus.clone());
    }
    Ok(seen.into_iter().collect())
}
