//! Identifier and literal helpers shared across the emit submodules.

use proc_macro2::{Ident, Literal, Span, TokenStream};
use quote::quote;

use crate::pipeline::{Outlet, PipelineFile, TapStreamKind};

pub(crate) fn snake_ident(s: &str) -> Ident {
    let snake = s.replace('-', "_");
    Ident::new(&snake, Span::call_site())
}

pub(crate) fn pascal_case(s: &str) -> String {
    s.split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect()
}

pub(crate) fn tap_static_ident(name: &str) -> Ident {
    let upper = name.to_uppercase().replace('-', "_");
    Ident::new(&format!("TAP_{upper}"), Span::call_site())
}

pub(crate) fn bus_static_ident(bus_id: &str) -> Ident {
    let upper = bus_id.to_uppercase().replace('-', "_");
    Ident::new(&format!("{upper}_BUS"), Span::call_site())
}

pub(crate) fn outlet_static_ident(name: &str) -> Ident {
    let upper = name.to_uppercase().replace('-', "_");
    Ident::new(&format!("OUTLET_{upper}"), Span::call_site())
}

pub(crate) fn rust_type_tokens(type_name: &str) -> TokenStream {
    match type_name {
        "f32" => quote!(f32),
        "f64" => quote!(f64),
        "u8" => quote!(u8),
        "u16" => quote!(u16),
        "u32" => quote!(u32),
        "u64" => quote!(u64),
        "i32" => quote!(i32),
        "i64" => quote!(i64),
        "bool" => quote!(bool),
        "usize" => quote!(usize),
        "ThresholdAlarm" => quote!(signal_layer_types::ThresholdAlarm),
        "DriverHealth" => quote!(signal_layer_types::DriverHealth),
        "HealthEvent" => quote!(signal_layer_types::HealthEvent),
        "DigitalState" => quote!(signal_layer_types::DigitalState),
        "PwmDuty" => quote!(signal_layer_types::PwmDuty),
        "OutletFault" => quote!(signal_layer_types::OutletFault),
        other => {
            let ident = Ident::new(other, Span::call_site());
            quote!(#ident)
        }
    }
}

/// Resolve the source id that ultimately feeds `input` — a `"src.field"`
/// reference or a step id, followed through the step chain to its rooting
/// source. Returns `None` if the chain doesn't root at a source.
pub(crate) fn input_root_source<'a>(input: &'a str, pipeline: &'a PipelineFile) -> Option<&'a str> {
    let mut cur = input;
    // Bounded by the step count (the graph is acyclic — validated) plus one.
    for _ in 0..=pipeline.steps.len() {
        if let Some((src, _field)) = cur.split_once('.') {
            return Some(src);
        }
        let step = pipeline.steps.iter().find(|s| s.id == cur)?;
        cur = step.input.as_str();
    }
    None
}

/// The feed-forward (pipeline-driven) outlets whose input roots at `source_id`,
/// in pipeline declaration order. These are applied inline in that source's task.
pub(crate) fn owned_outlets_for_source<'a>(
    source_id: &str,
    pipeline: &'a PipelineFile,
) -> Vec<&'a Outlet> {
    pipeline
        .outlets
        .iter()
        .filter(|o| o.input.is_some())
        .filter(|o| {
            o.input
                .as_deref()
                .and_then(|i| input_root_source(i, pipeline))
                == Some(source_id)
        })
        .collect()
}

pub(crate) fn stream_kind_tokens(kind: &TapStreamKind) -> TokenStream {
    match kind {
        TapStreamKind::Metric => quote!(Metric),
        TapStreamKind::Signal => quote!(Signal),
    }
}

// All config values are range-validated by validate_config_value() before codegen runs,
// so narrowing casts (u64→u8/u16/u32/usize, f64→f32) are known-safe at this point.
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn config_value_tokens(
    value: &serde_yaml::Value,
    rust_type: &str,
    crate_path: &Ident,
) -> anyhow::Result<TokenStream> {
    let ts = match rust_type {
        "u8" => {
            let v = value.as_u64().unwrap_or(0) as u8;
            let lit = Literal::u8_suffixed(v);
            quote!(#lit)
        }
        "u16" => {
            let v = value.as_u64().unwrap_or(0) as u16;
            let lit = Literal::u16_suffixed(v);
            quote!(#lit)
        }
        "u32" => {
            let v = value.as_u64().unwrap_or(0) as u32;
            let lit = Literal::u32_suffixed(v);
            quote!(#lit)
        }
        "u64" => {
            let v = value.as_u64().unwrap_or(0);
            let lit = Literal::u64_suffixed(v);
            quote!(#lit)
        }
        "usize" => {
            let v = value.as_u64().unwrap_or(0) as usize;
            let lit = Literal::usize_suffixed(v);
            quote!(#lit)
        }
        "f32" => {
            let v = value.as_f64().unwrap_or(0.0) as f32;
            let lit = Literal::f32_suffixed(v);
            quote!(#lit)
        }
        "f64" => {
            let v = value.as_f64().unwrap_or(0.0);
            let lit = Literal::f64_suffixed(v);
            quote!(#lit)
        }
        "bool" => {
            let v = value.as_bool().unwrap_or(false);
            if v { quote!(true) } else { quote!(false) }
        }
        // Non-primitive rust_type → enum variant. validate_config_value() has already
        // checked that `value` is a valid Rust identifier string.
        enum_type => {
            let variant_str = value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("enum field `{enum_type}`: expected a variant name string, got a non-string value"))?;
            let ty = Ident::new(enum_type, Span::call_site());
            let variant = Ident::new(variant_str, Span::call_site());
            quote!(#crate_path::#ty::#variant)
        }
    };
    Ok(ts)
}
