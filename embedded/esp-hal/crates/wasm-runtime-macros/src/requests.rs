//! Handling of `requests!` macro
use proc_macro2::TokenStream;
use quote::quote;
use syn::{
    Ident, Token,
    parse::{Parse, ParseStream},
};

use crate::entry::Entry;

mod kw {
    syn::custom_keyword!(wrap);
    syn::custom_keyword!(unwrap);
}

/// Parsed input for the macro
pub(crate) struct RequestsInput {
    pub inner_req: Ident,
    pub outer_req: Ident,
    pub outer_variant: Ident,
    pub outer_resp: Ident,
    pub outer_resp_variant: Ident,
    pub inner_resp: Ident,
    pub entries: Vec<Entry>,
}

impl Parse for RequestsInput {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        // wrap(InnerReq => OuterReq::OuterVariant)
        input.parse::<kw::wrap>()?;
        let wrap_content;
        syn::parenthesized!(wrap_content in input);
        let inner_req: Ident = wrap_content.parse()?;
        wrap_content.parse::<Token![=>]>()?;
        let outer_req: Ident = wrap_content.parse()?;
        wrap_content.parse::<Token![::]>()?;
        let outer_variant: Ident = wrap_content.parse()?;

        input.parse::<Token![,]>()?;

        // unwrap(OuterResp::OuterRespVariant => InnerResp)
        input.parse::<kw::unwrap>()?;
        let unwrap_content;
        syn::parenthesized!(unwrap_content in input);
        let outer_resp: Ident = unwrap_content.parse()?;
        unwrap_content.parse::<Token![::]>()?;
        let outer_resp_variant: Ident = unwrap_content.parse()?;
        unwrap_content.parse::<Token![=>]>()?;
        let inner_resp: Ident = unwrap_content.parse()?;

        input.parse::<Token![;]>()?;

        let mut entries = Vec::new();
        while !input.is_empty() {
            entries.push(input.parse::<Entry>()?);
        }

        Ok(RequestsInput {
            inner_req,
            outer_req,
            outer_variant,
            outer_resp,
            outer_resp_variant,
            inner_resp,
            entries,
        })
    }
}

/// Generates all code for `requests!`.
pub(crate) fn codegen(input: RequestsInput) -> TokenStream {
    let RequestsInput {
        inner_req,
        outer_req,
        outer_variant,
        outer_resp,
        outer_resp_variant,
        inner_resp,
        entries,
    } = input;

    let req_variants = entries.iter().map(|e| e.request_variant(&inner_req));
    let resp_variants = entries.iter().map(Entry::response_variant);

    let struct_defs: Vec<_> = entries
        .iter()
        .filter(|e| !matches!(e, Entry::Category { .. }))
        .map(Entry::struct_def)
        .collect();

    let entry_impls: Vec<_> = entries
        .iter()
        .filter(|e| !matches!(e, Entry::Category { .. }))
        .map(|e| {
            e.from_and_typed_request(
                &outer_variant,
                &inner_req,
                &outer_req,
                &outer_resp,
                &outer_resp_variant,
                &inner_resp,
            )
        })
        .collect();

    quote! {
        #[derive(Debug)]
        pub enum #inner_req { #(#req_variants),* }

        #[derive(Debug)]
        pub enum #inner_resp { #(#resp_variants),* }

        #[allow(non_snake_case)]
        pub mod #outer_variant {
            #[allow(unused_imports)]
            use super::*;
            #(#struct_defs)*
        }

        #(#entry_impls)*

        // Terminal hop: `#inner_req` → `#outer_req` and `Response → #outer_resp → #inner_resp`.
        // These let `#inner_req` participate in `.into()` chains up to `Request`, and let
        // `#inner_resp` walk down from `Response`. Each `requests!` provides its own hop;
        // chains compose through `From` / `ExtractFromResponse`.
        impl ::core::convert::From<#inner_req> for #outer_req {
            fn from(req: #inner_req) -> #outer_req {
                #outer_req::#outer_variant(req)
            }
        }
        impl crate::async_request::ExtractFromResponse for #inner_resp {
            fn extract_from_response(resp: crate::async_request::Response) -> Self {
                let outer = <#outer_resp as crate::async_request::ExtractFromResponse>::extract_from_response(resp);
                let #outer_resp::#outer_resp_variant(v) = outer else { unreachable!() };
                v
            }
        }
    }
}
