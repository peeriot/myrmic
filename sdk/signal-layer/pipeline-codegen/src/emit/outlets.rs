//! Outlet slot statics and the outlet registry setup — the write-side mirror of
//! `taps.rs`.

use proc_macro2::TokenStream;
use quote::quote;

use crate::ChipBackend;
use crate::pipeline::PipelineFile;

use super::helpers::{outlet_static_ident, rust_type_tokens};

/// Emit one `RetainedSlot` static per outlet. Outlets are retained-only for v1
/// (a command is last-value-wins), so no kind/stream selection is needed.
pub(crate) fn emit_outlet_statics(pipeline: &PipelineFile) -> TokenStream {
    let mut ts = TokenStream::new();
    // Only cell-driven outlets get a slot + registry entry. Pipeline-driven
    // (feed-forward) outlets are applied inline in the source task and are never
    // exposed to WASM cells.
    for outlet in pipeline.outlets.iter().filter(|o| o.input.is_none()) {
        let static_name = outlet_static_ident(&outlet.name);
        let ty = rust_type_tokens(&outlet.type_name);
        ts.extend(quote! {
            pub static #static_name: RetainedSlot<#ty> = RetainedSlot::new();
        });
    }
    ts
}

/// Emit `register_outlets(&mut OutletRegistry)`, inserting every generated
/// outlet slot into the registry by name so WASM cells can resolve them.
pub(crate) fn emit_register_outlets(pipeline: &PipelineFile) -> TokenStream {
    let mut regs = TokenStream::new();
    for outlet in pipeline.outlets.iter().filter(|o| o.input.is_none()) {
        let name = &outlet.name;
        let static_name = outlet_static_ident(&outlet.name);
        regs.extend(quote! {
            registry.register(#name, OutletEntry::retained(&#static_name))?;
        });
    }

    quote! {
        /// Register every pipeline outlet into the host outlet registry by name.
        pub fn register_outlets(registry: &mut OutletRegistry) -> Result<(), TapError> {
            #regs
            Ok(())
        }
    }
}

/// Emit `setup_outlet_registry() -> usize`, the single entry point firmware
/// calls to build the outlet registry and hand it to the WASM runtime. Always
/// emitted (even with no outlets) so the firmware call site is stable, mirroring
/// `setup_tap_registry`.
pub(crate) fn emit_setup_outlet_registry(backend: &dyn ChipBackend) -> TokenStream {
    let outlet_handoff = backend.emit_outlet_handoff();
    quote! {
        /// Build the outlet registry, register all pipeline outlets, and hand it
        /// to the WASM runtime. Returns the number of outlets registered.
        /// Called exactly once from the firmware entry point before WASM starts.
        pub fn setup_outlet_registry() -> usize {
            let mut registry = OutletRegistry::new();
            register_outlets(&mut registry).expect("outlet registry full");
            let count = registry.len();
            #outlet_handoff
            count
        }
    }
}
