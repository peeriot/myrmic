//! Trait-shape conformance test (SR-5).
//!
//! A test backend overrides exactly the thirteen runtime hooks with the D5 v8
//! signatures; compilation is the assertion. Adding, removing, or renaming a
//! hook must fail to compile.

use indexmap::IndexMap;
use pipeline_backend_api::{ChipBackend, descriptor::DriverSchema, manifest::BoardManifest};
use proc_macro2::TokenStream;
use quote::quote;

struct ConformanceBackend;

impl ChipBackend for ConformanceBackend {
    // ── Required non-defaulted methods ──────────────────────────────────────

    fn emit_imports(&self) -> TokenStream {
        quote! {}
    }

    fn emit_board_peripherals(
        &self,
        _manifest: &BoardManifest,
        _driver_schemas: &IndexMap<String, DriverSchema>,
    ) -> TokenStream {
        quote! {}
    }

    fn i2c_bus_type(&self) -> TokenStream {
        quote! { MockI2c }
    }

    fn spi_bus_type(&self) -> TokenStream {
        panic!("no SPI")
    }

    fn spi_cs_type(&self) -> TokenStream {
        panic!("no SPI")
    }

    fn gpio_flex_type(&self) -> TokenStream {
        quote! { MockFlex }
    }

    fn emit_pipeline_pins_macro(&self, _manifest: &BoardManifest) -> TokenStream {
        // Return empty: this platform has no pin-to-WASM forwarding.
        quote! {}
    }

    // ── Thirteen runtime hooks — overriding every hook pins the exact D5 v8 signatures ──

    fn emit_runtime_imports(&self) -> TokenStream {
        quote! {}
    }

    fn emit_task_attribute(&self) -> TokenStream {
        quote! {}
    }

    fn emit_interval(&self, ms: u64) -> TokenStream {
        let _ = ms;
        quote! {}
    }

    fn emit_now_millis(&self) -> TokenStream {
        quote! {}
    }

    fn emit_spawn(&self, task: &TokenStream, label: &str) -> TokenStream {
        let _ = task;
        let _ = label;
        quote! {}
    }

    fn emit_tap_handoff(&self) -> TokenStream {
        quote! {}
    }

    fn emit_bus_static(&self, bus: &TokenStream, inner: &TokenStream) -> TokenStream {
        let _ = bus;
        let _ = inner;
        quote! {}
    }

    fn emit_bus_device_new(&self, bus: &TokenStream) -> TokenStream {
        let _ = bus;
        quote! {}
    }

    fn emit_spi_bus_device_new(&self, bus: &TokenStream, cs: &TokenStream) -> TokenStream {
        let _ = bus;
        let _ = cs;
        quote! {}
    }

    fn emit_bus_device_type(&self, inner: &TokenStream) -> TokenStream {
        let _ = inner;
        quote! {}
    }

    fn emit_spi_bus_device_type(&self, inner: &TokenStream, cs: &TokenStream) -> TokenStream {
        let _ = inner;
        let _ = cs;
        quote! {}
    }

    fn emit_bus_init(
        &self,
        bus_var: &TokenStream,
        static_ident: &TokenStream,
        bus_field: &TokenStream,
        inner: &TokenStream,
    ) -> TokenStream {
        let _ = bus_var;
        let _ = static_ident;
        let _ = bus_field;
        let _ = inner;
        quote! {}
    }

    fn emit_outlet_handoff(&self) -> TokenStream {
        quote! {}
    }
}

/// Compilation is the assertion: if `ConformanceBackend` implements `ChipBackend`
/// without errors, all thirteen hooks have the correct signatures.
#[test]
fn trait_shape_compiles() {
    let _b: &dyn ChipBackend = &ConformanceBackend;
}
