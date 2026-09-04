//! Handling of `root_requests!` macro
use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};

use crate::entry::{Entry, RespKind};

/// Parsed input of the macro
pub(crate) struct RootRequestsInput {
    pub entries: Vec<Entry>,
}

impl Parse for RootRequestsInput {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut entries = Vec::new();
        while !input.is_empty() {
            entries.push(input.parse::<Entry>()?);
        }
        Ok(RootRequestsInput { entries })
    }
}

/// Generates all code for the macro
pub(crate) fn codegen(input: RootRequestsInput) -> TokenStream {
    let RootRequestsInput { entries } = input;

    let req_variants = entries.iter().map(root_req_variant);
    let resp_variants = entries.iter().map(root_resp_variant);

    let items: Vec<_> = entries
        .iter()
        .filter(|e| !matches!(e, Entry::Category { .. }))
        .map(root_flat_item)
        .collect();

    quote! {
        pub enum Request  { #(#req_variants),* }
        pub enum Response { #(#resp_variants),* }

        // Identity: the top-level `Response` is the base case for chained extraction.
        impl crate::async_request::ExtractFromResponse for Response {
            fn extract_from_response(resp: Response) -> Self { resp }
        }

        #(#items)*
    }
}

fn root_req_variant(e: &Entry) -> TokenStream {
    let attrs = e.attrs();
    match e {
        Entry::Unit { name, .. } => quote! { #(#attrs)* #name },
        Entry::Tuple { name, field, .. } => quote! { #(#attrs)* #name(#field) },
        Entry::Struct { name, fields, .. } => {
            let fs = fields.iter().map(|(f, t)| quote! { #f: #t });
            quote! { #(#attrs)* #name { #(#fs),* } }
        }
        Entry::Category { name, req_ty, .. } => quote! { #(#attrs)* #name(#req_ty) },
    }
}

fn root_resp_variant(e: &Entry) -> TokenStream {
    let attrs = e.attrs();
    match e {
        Entry::Unit { name, resp, .. }
        | Entry::Tuple { name, resp, .. }
        | Entry::Struct { name, resp, .. } => match resp {
            RespKind::Unit => quote! { #(#attrs)* #name },
            RespKind::Ty(ty) => quote! { #(#attrs)* #name(#ty) },
        },
        Entry::Category { name, resp_ty, .. } => quote! { #(#attrs)* #name(#resp_ty) },
    }
}

fn root_flat_item(e: &Entry) -> TokenStream {
    let attrs = e.attrs();
    let (name, struct_def, into_req_body) = match e {
        Entry::Unit { name, .. } => {
            let s = quote! { pub struct #name; };
            let b = quote! { Request::#name };
            (name, s, b)
        }
        Entry::Tuple { name, field, .. } => {
            let s = quote! { pub struct #name(pub #field); };
            let b = quote! { Request::#name(self.0) };
            (name, s, b)
        }
        Entry::Struct { name, fields, .. } => {
            let pub_fields = fields.iter().map(|(f, t)| quote! { pub #f: #t });
            let s = quote! { pub struct #name { #(#pub_fields),* } };
            let field_names: Vec<_> = fields.iter().map(|(f, _)| f).collect();
            let b = quote! { Request::#name { #(#field_names: self.#field_names),* } };
            (name, s, b)
        }
        #[expect(
            clippy::unreachable,
            reason = "Category entries are explicitly excluded before root_flat_item is ever called on them"
        )]
        Entry::Category { .. } => unreachable!("Broken proc macro logic"),
    };

    let (resp_ty, extract_fn): (TokenStream, TokenStream) = match e {
        Entry::Unit {
            resp: RespKind::Unit,
            ..
        }
        | Entry::Tuple {
            resp: RespKind::Unit,
            ..
        }
        | Entry::Struct {
            resp: RespKind::Unit,
            ..
        } => (
            quote! { () },
            quote! { fn extract_response(_: Response) -> () {} },
        ),
        Entry::Unit {
            resp: RespKind::Ty(ty),
            ..
        }
        | Entry::Tuple {
            resp: RespKind::Ty(ty),
            ..
        }
        | Entry::Struct {
            resp: RespKind::Ty(ty),
            ..
        } => (
            quote! { #ty },
            quote! {
                fn extract_response(resp: Response) -> #ty {
                    let Response::#name(v) = resp else { unreachable!() };
                    v
                }
            },
        ),
        #[expect(
            clippy::unreachable,
            reason = "Category entries are explicitly excluded before root_flat_item is ever called on them"
        )]
        Entry::Category { .. } => unreachable!("Broken proc-macro logic"),
    };

    quote! {
        #(#attrs)*
        #struct_def

        #(#attrs)*
        impl crate::async_request::TypedRequest for #name {
            type Response = #resp_ty;

            fn into_request(self) -> Request { #into_req_body }

            #extract_fn
        }
    }
}
