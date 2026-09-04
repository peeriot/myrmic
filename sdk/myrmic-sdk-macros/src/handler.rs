//! Codegen shared by the `#[cmd]` and `#[evt]` handler macros.
//!
//! Both expand a free function into a Wasm export the host calls at runtime.
//! The export receives the cell's own identity and the sender's identity —
//! each a 128-bit UUID split into two `i64` halves — followed by the argument
//! buffer size. The generated glue recombines the halves into a `Metadata`,
//! reads and decodes the argument buffer via the parameter's `Decoder` impl,
//! and forwards both to the annotated function.
//!
//! The function takes a leading `Metadata`, optionally followed by the payload
//! `Decoder`. When the `Decoder` is omitted it defaults to `Void`, which
//! rejects any non-empty payload. It must return `myrmic_sdk::Result<_>`.

use proc_macro2::TokenStream;
use quote::{format_ident, quote_spanned};
use syn::{
    Expr, ExprLit, FnArg, ItemFn, Lit, LitStr, Meta, PatType, Token, parse::Parser,
    punctuated::Punctuated, spanned::Spanned,
};

/// Which handler kind is being generated — selects the export name
/// (`command_<name>`, `event_<name>`, or the fixed `on_cell_lost`).
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Command,
    Event,
    /// The reserved child-loss notification handler. Exactly one per cell,
    /// exported under the fixed name `on_cell_lost`.
    Monitor,
}

impl Kind {
    fn prefix(self) -> &'static str {
        match self {
            Kind::Command => "command",
            Kind::Event => "event",
            Kind::Monitor => "on",
        }
    }

    fn arg_desc(self) -> &'static str {
        match self {
            Kind::Command => "#[cmd]",
            Kind::Event => "#[evt]",
            Kind::Monitor => "#[monitor]",
        }
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn handler_impl(
    attr: TokenStream,
    item: TokenStream,
    root: &TokenStream,
    kind: Kind,
) -> TokenStream {
    let name_override = match parse_name_override(attr) {
        Ok(name) => name,
        Err(err) => return err.to_compile_error(),
    };

    let func: ItemFn = match syn::parse2(item) {
        Ok(func) => func,
        Err(err) => return err.to_compile_error(),
    };

    let fn_ident = func.sig.ident.clone();
    let vis = func.vis.clone();
    let span = func.sig.span();

    // The export symbol is `<prefix>_<name>`; the host derives the command/event
    // name by stripping the prefix. `#[cmd(name = "...")]` overrides the `<name>`
    // half, decoupling the wire name from the Rust function's identifier. The
    // monitor export is the fixed reserved name the host routes cell_lost to.
    let (name, name_span) = match kind {
        Kind::Monitor => {
            if let Some(lit) = &name_override {
                return syn::Error::new_spanned(
                    lit,
                    "#[monitor] exports the fixed name `on_cell_lost`; `name` cannot be overridden",
                )
                .to_compile_error();
            }
            ("cell_lost".to_owned(), span)
        }
        _ => match &name_override {
            Some(lit) => (lit.value(), lit.span()),
            None => (fn_ident.to_string(), span),
        },
    };
    let export_name = format_ident!("{}_{}", kind.prefix(), name, span = name_span);
    let impl_name = format_ident!("__{}_{}_impl", kind.prefix(), fn_ident, span = span);

    let params: Vec<&PatType> = func
        .sig
        .inputs
        .iter()
        .filter_map(|arg| match arg {
            FnArg::Typed(pt) => Some(pt),
            FnArg::Receiver(_) => None,
        })
        .collect();

    // `Metadata` is always required. The payload `Decoder` is optional; when
    // omitted it defaults to `Void`, which rejects any non-empty payload.
    // `arg_ty` is the same type, surfaced as the marker's `Handler::Arg`.
    let (decode_and_call, arg_ty) = match params.as_slice() {
        [_meta] => (
            quote_spanned! { span =>
                <#root::Void as #root::Decoder>::from_args(arg_size as usize)?;
                #fn_ident(__md)?;
            },
            quote_spanned! { span => #root::Void },
        ),
        [_meta, decoder] => {
            let decoder_ty = &decoder.ty;
            (
                quote_spanned! { span =>
                    let __arg = <#decoder_ty as #root::Decoder>::from_args(arg_size as usize)?;
                    #fn_ident(__md, __arg)?;
                },
                quote_spanned! { span => #decoder_ty },
            )
        }
        _ => {
            return syn::Error::new(
                span,
                format!(
                    "{} requires `(Metadata)` or `(Metadata, Decoder)`, e.g. `(md: Metadata, msg: ServerMessage)`",
                    kind.arg_desc()
                ),
            )
            .to_compile_error();
        }
    };

    // Only command handlers get a `Handler` marker. Its sole purpose is to be a
    // `Callback::of::<H>()` target, and callbacks invoke a command back on an
    // `Sri` — event handlers are pub/sub and can never be callback targets, so
    // generating the marker for them would only enable a nonsensical callback.
    //
    // A distinct nominal marker type per handler: two handlers with identical
    // signatures still get distinct types, so `impl Handler` never collides.
    // Braced-empty so it lives only in the type namespace and coexists with the
    // like-named function.
    let name_lit = LitStr::new(&name, name_span);
    let marker = match kind {
        Kind::Command => quote_spanned! { span =>
            // The marker is a type-level handle for `Callback::of::<H>()`; a
            // command that is never a callback target leaves it unconstructed.
            #[allow(non_camel_case_types, dead_code)]
            #vis struct #fn_ident {}

            impl #root::Handler for #fn_ident {
                const NAME: &'static str = #name_lit;
                type Arg = #arg_ty;
            }
        },
        Kind::Event | Kind::Monitor => quote_spanned! { span => },
    };

    quote_spanned! { span =>
        #func

        #marker

        #[unsafe(no_mangle)]
        pub extern "C" fn #export_name(
            id_hi: i64,
            id_lo: i64,
            sender_hi: i64,
            sender_lo: i64,
            arg_size: i32,
        ) -> i32 {
            let md = #root::Metadata::from_parts(id_hi, id_lo, sender_hi, sender_lo);

            match #impl_name(md, arg_size) {
                Ok(()) => 0,
                Err(__msg) => {
                    #root::report_error(__msg).unwrap();
                    #root::error!("{__msg}").unwrap();
                    -1
                }
            }
        }

        // The allow is scoped to generated code only: `arg_size` comes from
        // the host, which passes a non-negative payload length, so the
        // `as usize` in the decode cannot lose the sign.
        #[allow(clippy::cast_sign_loss)]
        fn #impl_name(
            __md: #root::Metadata,
            arg_size: i32,
        ) -> #root::Result<()> {
            #decode_and_call
            Ok(())
        }
    }
}

/// Parses the macro's attribute arguments, extracting an optional
/// `name = "..."` override for the export symbol's name component. Returns
/// `None` when no attribute is supplied.
fn parse_name_override(attr: TokenStream) -> syn::Result<Option<LitStr>> {
    if attr.is_empty() {
        return Ok(None);
    }

    let metas = Punctuated::<Meta, Token![,]>::parse_terminated.parse2(attr)?;
    let mut name = None;
    for meta in metas {
        if !meta.path().is_ident("name") {
            return Err(syn::Error::new_spanned(
                meta.path(),
                "unknown attribute argument; expected `name = \"...\"`",
            ));
        }
        if name.is_some() {
            return Err(syn::Error::new_spanned(meta, "duplicate `name` argument"));
        }
        let value = match meta {
            Meta::NameValue(nv) => nv.value,
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    "`name` must be given as `name = \"...\"`",
                ));
            }
        };
        let lit = match value {
            Expr::Lit(ExprLit {
                lit: Lit::Str(lit), ..
            }) => lit,
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    "`name` must be a string literal",
                ));
            }
        };
        validate_name(&lit)?;
        name = Some(lit);
    }
    Ok(name)
}

/// A `name` override becomes the tail of an `extern "C"` symbol, so it must be a
/// valid function-name component: non-empty ASCII alphanumerics/underscores.
/// This mirrors the host-side check in `myrmic_common::cells::names`.
fn validate_name(lit: &LitStr) -> syn::Result<()> {
    let value = lit.value();
    if value.is_empty() {
        return Err(syn::Error::new_spanned(lit, "`name` cannot be empty"));
    }
    if !value.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(syn::Error::new_spanned(
            lit,
            "`name` may only contain ASCII alphanumeric characters and underscores",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    fn expand(kind: Kind) -> String {
        let item = quote! {
            fn handle(md: myrmic_sdk::Metadata, msg: Payload) -> myrmic_sdk::Result<()> { Ok(()) }
        };
        handler_impl(TokenStream::new(), item, &quote!(::myrmic_sdk), kind).to_string()
    }

    #[test]
    fn command_emits_callback_marker() {
        let out = expand(Kind::Command);
        assert!(
            out.contains("struct handle"),
            "cmd should emit a marker struct:\n{out}"
        );
        assert!(
            out.contains("Handler for handle"),
            "cmd should impl Handler:\n{out}"
        );
        assert!(
            out.contains("command_handle"),
            "cmd should emit its export:\n{out}"
        );
    }

    #[test]
    fn event_omits_callback_marker() {
        // Events can't be `Callback` targets, so `#[evt]` must not generate the
        // marker struct or `Handler` impl — only the export + impl fn.
        let out = expand(Kind::Event);
        assert!(
            !out.contains("struct handle"),
            "evt must not emit a marker struct:\n{out}"
        );
        assert!(
            !out.contains("Handler for handle"),
            "evt must not impl Handler:\n{out}"
        );
        assert!(
            out.contains("event_handle"),
            "evt should still emit its export:\n{out}"
        );
    }
}

#[cfg(test)]
mod monitor_tests {
    use super::*;
    use quote::quote;

    #[test]
    fn monitor_emits_fixed_export_without_marker() {
        let item = quote! {
            fn lost(md: myrmic_sdk::Metadata, l: CellLost) -> myrmic_sdk::Result<()> { Ok(()) }
        };
        let out = handler_impl(
            TokenStream::new(),
            item,
            &quote!(::myrmic_sdk),
            Kind::Monitor,
        )
        .to_string();
        assert!(out.contains("on_cell_lost"), "{out}");
        assert!(!out.contains("struct lost"), "{out}");
    }

    #[test]
    fn monitor_rejects_name_override() {
        let item = quote! {
            fn lost(md: myrmic_sdk::Metadata) -> myrmic_sdk::Result<()> { Ok(()) }
        };
        let out = handler_impl(
            quote! { name = "other" },
            item,
            &quote!(::myrmic_sdk),
            Kind::Monitor,
        )
        .to_string();
        assert!(out.contains("cannot be overridden"), "{out}");
    }
}
