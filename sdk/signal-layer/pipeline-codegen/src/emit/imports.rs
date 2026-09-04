//! Common and chip-specific `use` statements at the top of the generated file.

use proc_macro2::TokenStream;
use quote::quote;

use crate::ChipBackend;

pub(crate) fn emit_common_imports(backend: &dyn ChipBackend) -> TokenStream {
    let chip_imports = backend.emit_imports();
    let runtime_imports = backend.emit_runtime_imports();
    quote! {
        #![allow(unused_imports, dead_code, unused_variables)]

        use signal_layer_core::{
            ProcessingStep, EventSlot, Metric, OutletEntry, OutletRegistry, RetainedSlot, Signal,
            SlotEntry, TapError, TapRegistry, Timestamp,
        };
        use signal_layer_types::{DriverHealth, HealthEvent};
        #runtime_imports

        #chip_imports
    }
}
