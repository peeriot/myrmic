//! The `#[init]` cell-initializer macro.
//!
//! Expands a free function into the `init_cell` Wasm export the host calls once
//! when a cell is first deployed. The export receives the cell's own identity
//! and the spawner's identity (each a 128-bit UUID split into two `i64`
//! halves), followed by the argument-buffer size. The glue recombines the
//! halves into a `Metadata` and decodes the argument buffer via the payload
//! parameter's `Decoder` impl.
//!
//! The function takes a leading `Metadata`, optionally followed by a payload
//! `Decoder`, and must return `myrmic_sdk::Result<_>`. When the payload
//! parameter is omitted it defaults to `Void`, which rejects any non-empty
//! payload — so spawning with a payload against a cell whose init takes none
//! fails rather than silently dropping it. The payload is supplied at spawn
//! time via `ClassHandle::spawn_with`. Init runs setup only — durable state
//! lives in the data layer, not cell state.

use proc_macro2::TokenStream;
use quote::quote_spanned;
use syn::{FnArg, ItemFn, PatType, spanned::Spanned};

pub(crate) fn init_impl(item: TokenStream, root: &TokenStream) -> TokenStream {
    let func: ItemFn = match syn::parse2(item) {
        Ok(func) => func,
        Err(err) => return err.to_compile_error(),
    };

    let fn_ident = func.sig.ident.clone();
    let span = func.sig.span();

    let params: Vec<&PatType> = func
        .sig
        .inputs
        .iter()
        .filter_map(|arg| match arg {
            FnArg::Typed(pt) => Some(pt),
            FnArg::Receiver(_) => None,
        })
        .collect();

    // `Metadata` is always required; the SRI halves are therefore always read.
    // `arg_size` is always consumed (to decode or reject the payload).
    let md = quote_spanned! { span =>
        #root::Metadata::from_parts(__id_hi, __id_lo, __sender_hi, __sender_lo)
    };

    let call = match params.as_slice() {
        [_meta] => quote_spanned! { span =>
            <#root::Void as #root::Decoder>::from_args(__arg_size as usize)?;
            #fn_ident(#md)?;
        },
        [_meta, decoder] => {
            let decoder_ty = &decoder.ty;
            quote_spanned! { span =>
                let __arg = <#decoder_ty as #root::Decoder>::from_args(__arg_size as usize)?;
                #fn_ident(#md, __arg)?;
            }
        }
        _ => {
            return syn::Error::new(
                span,
                "#[init] requires `(Metadata)` or `(Metadata, Decoder)`, e.g. `(md: Metadata)`",
            )
            .to_compile_error();
        }
    };

    quote_spanned! { span =>
        #func

        #[unsafe(no_mangle)]
        pub extern "C" fn init_cell(
            __id_hi: i64,
            __id_lo: i64,
            __sender_hi: i64,
            __sender_lo: i64,
            __arg_size: i32,
        ) -> i32 {
            match __cell_init_impl(__id_hi, __id_lo, __sender_hi, __sender_lo, __arg_size) {
                Ok(()) => 0,
                Err(__err) => {
                    let __err_msg = #root::format!("Cell init error: {__err}");
                    #root::report_error(&__err_msg).unwrap();
                    #root::error!("{__err_msg}").unwrap();
                    -1
                }
            }
        }

        // The allow is scoped to generated code only: `__arg_size` comes from
        // the host, which passes a non-negative payload length, so the
        // `as usize` in the decode cannot lose the sign.
        #[allow(clippy::cast_sign_loss)]
        fn __cell_init_impl(
            __id_hi: i64,
            __id_lo: i64,
            __sender_hi: i64,
            __sender_lo: i64,
            __arg_size: i32,
        ) -> #root::Result<()> {
            #call
            Ok(())
        }
    }
}
