//! Tap slot statics, including the auto-registered shared health tap.

use anyhow::{Result, anyhow};
use proc_macro2::TokenStream;
use quote::quote;

use crate::ChipBackend;
use crate::pipeline::{PipelineFile, TapKind};

use super::helpers::{rust_type_tokens, stream_kind_tokens, tap_static_ident};

pub(crate) fn emit_tap_statics(pipeline: &PipelineFile) -> Result<TokenStream> {
    let mut ts = TokenStream::new();

    // Auto-register the shared health event tap whenever there is at least one source.
    // One tap for all sources — keeps MAX_TAPS headroom regardless of source count.
    if !pipeline.sources.is_empty() {
        ts.extend(quote! {
            pub static TAP__SIGNAL_LAYER_HEALTH: EventSlot<HealthEvent> = EventSlot::new();
        });
    }

    for tap in &pipeline.taps {
        let static_name = tap_static_ident(&tap.name);
        let ty = rust_type_tokens(&tap.type_name);
        let decl = match tap.kind {
            TapKind::Retained => {
                let kind = stream_kind_tokens(&tap.stream_kind);
                quote! {
                    pub static #static_name: RetainedSlot<#ty, #kind> = RetainedSlot::new();
                }
            }
            TapKind::Event => {
                quote! {
                    pub static #static_name: EventSlot<#ty> = EventSlot::new();
                }
            }
            TapKind::Batch => {
                return Err(anyhow!(
                    "tap `{}`: batch taps not yet supported by codegen",
                    tap.name
                ));
            }
        };
        ts.extend(decl);
    }
    Ok(ts)
}

/// Emit `register_taps(&mut TapRegistry)`, which inserts every generated static
/// slot (and the health tap) into the registry by name so WASM cells can
/// resolve them at runtime.
pub(crate) fn emit_register_taps(pipeline: &PipelineFile) -> Result<TokenStream> {
    let mut regs = TokenStream::new();

    if !pipeline.sources.is_empty() {
        regs.extend(quote! {
            registry.register("_signal_layer_health", SlotEntry::event(&TAP__SIGNAL_LAYER_HEALTH))?;
        });
    }

    for tap in &pipeline.taps {
        let name = &tap.name;
        let static_name = tap_static_ident(&tap.name);
        match tap.kind {
            TapKind::Retained => regs.extend(quote! {
                registry.register(#name, SlotEntry::retained(&#static_name))?;
            }),
            TapKind::Event => regs.extend(quote! {
                registry.register(#name, SlotEntry::event(&#static_name))?;
            }),
            TapKind::Batch => {
                return Err(anyhow!(
                    "tap `{}`: batch taps not yet supported by codegen",
                    tap.name
                ));
            }
        }
    }

    Ok(quote! {
        /// Register every pipeline tap into the host tap registry by name.
        pub fn register_taps(registry: &mut TapRegistry) -> Result<(), TapError> {
            #regs
            Ok(())
        }
    })
}

/// Emit `setup_tap_registry() -> usize`, the single entry point that firmware
/// calls to build the registry, populate it, and hand it to the WASM runtime.
/// Centralising this in generated code means the runtime handoff
/// is only ever called from one place — structurally enforcing the once-before-
/// WAMR invariant without requiring unsafe guards.
pub(crate) fn emit_setup_tap_registry(backend: &dyn ChipBackend) -> TokenStream {
    let tap_handoff = backend.emit_tap_handoff();
    quote! {
        /// Build the tap registry, register all pipeline taps, and hand it to
        /// the WASM runtime. Returns the number of taps registered.
        /// Called exactly once from the firmware entry point before WASM starts.
        pub fn setup_tap_registry() -> usize {
            let mut registry = TapRegistry::new();
            register_taps(&mut registry).expect("tap registry full");
            let count = registry.len();
            #tap_handoff
            count
        }
    }
}
