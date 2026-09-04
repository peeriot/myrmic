//! Handles the entries to both `root_requests!` and `requests!` macro
use proc_macro2::TokenStream;
use quote::quote;
use syn::{
    Attribute, Ident, Token, Type,
    parse::{Parse, ParseStream},
    token,
};

mod kw {
    syn::custom_keyword!(category);
}

/// The response-side of an entry
#[expect(
    clippy::large_enum_variant,
    reason = "Async requests are short lived and we don't have many multiple requests. This means \
    that it's actually more beneficial to have this statically rather than boxed."
)]
pub(crate) enum RespKind {
    /// `=> ()` - the response carries no data.
    Unit,
    /// `=> T` - the response wraps `T`.
    Ty(Type),
}

/// A single entry in the `requests!` or `root_requests!`
pub(crate) enum Entry {
    /// `Name => RespKind` - unit struct request.
    Unit {
        attrs: Vec<Attribute>,
        name: Ident,
        resp: RespKind,
    },
    /// `Name(T) => RespKind` - newtype struct request.
    Tuple {
        attrs: Vec<Attribute>,
        name: Ident,
        field: Type,
        resp: RespKind,
    },
    /// `Name { f: T, .. } => RespKind` - named-fields struct request.
    Struct {
        attrs: Vec<Attribute>,
        name: Ident,
        fields: Vec<(Ident, Type)>,
        resp: RespKind,
    },
    /// `category Name(T) => U` - sub-tree passthrough.
    /// Adds a variant to both enums; the matching `requests!` invocation for `T`
    /// provides the wiring (`From<T> for ParentReq` and `ExtractFromResponse for U`).
    Category {
        attrs: Vec<Attribute>,
        name: Ident,
        req_ty: Type,
        resp_ty: Type,
    },
}

impl Parse for Entry {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let attrs = input.call(Attribute::parse_outer)?;
        for attr in &attrs {
            if !attr.path().is_ident("cfg") {
                return Err(syn::Error::new_spanned(
                    attr,
                    "only #[cfg(...)] attributes are supported on request entries",
                ));
            }
        }

        if input.peek(kw::category) {
            input.parse::<kw::category>()?;
            let name: Ident = input.parse()?;
            let content;
            syn::parenthesized!(content in input);
            let req_ty: Type = content.parse()?;
            input.parse::<Token![=>]>()?;
            let resp_ty: Type = input.parse()?;
            input.parse::<Token![,]>()?;
            return Ok(Entry::Category {
                attrs,
                name,
                req_ty,
                resp_ty,
            });
        }

        let name: Ident = input.parse()?;

        if input.peek(token::Paren) {
            let content;
            syn::parenthesized!(content in input);
            let field: Type = content.parse()?;
            let resp = parse_resp_kind(input)?;
            input.parse::<Token![,]>()?;
            return Ok(Entry::Tuple {
                attrs,
                name,
                field,
                resp,
            });
        }

        if input.peek(token::Brace) {
            let content;
            syn::braced!(content in input);
            let fields = parse_named_fields(&content)?;
            let resp = parse_resp_kind(input)?;
            input.parse::<Token![,]>()?;
            return Ok(Entry::Struct {
                attrs,
                name,
                fields,
                resp,
            });
        }

        let resp = parse_resp_kind(input)?;
        input.parse::<Token![,]>()?;
        Ok(Entry::Unit { attrs, name, resp })
    }
}

fn parse_resp_kind(input: ParseStream<'_>) -> syn::Result<RespKind> {
    input.parse::<Token![=>]>()?;
    let ty: Type = input.parse()?;
    Ok(match ty {
        Type::Tuple(ref t) if t.elems.is_empty() => RespKind::Unit,
        _ => RespKind::Ty(ty),
    })
}

fn parse_named_fields(input: ParseStream<'_>) -> syn::Result<Vec<(Ident, Type)>> {
    let mut fields = Vec::new();
    while !input.is_empty() {
        let name: Ident = input.parse()?;
        input.parse::<Token![:]>()?;
        let ty: Type = input.parse()?;
        fields.push((name, ty));
        if input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
        }
    }
    Ok(fields)
}

impl Entry {
    pub(crate) fn attrs(&self) -> &[Attribute] {
        match self {
            Entry::Unit { attrs, .. }
            | Entry::Tuple { attrs, .. }
            | Entry::Struct { attrs, .. }
            | Entry::Category { attrs, .. } => attrs,
        }
    }

    /// Generates the variant token for the inner request enum.
    pub(crate) fn request_variant(&self, _inner_req: &Ident) -> TokenStream {
        let attrs = self.attrs();
        match self {
            Entry::Unit { name, .. } => quote! { #(#attrs)* #name },
            Entry::Tuple { name, field, .. } => quote! { #(#attrs)* #name(#field) },
            Entry::Struct { name, fields, .. } => {
                let fs = fields.iter().map(|(f, t)| quote! { #f: #t });
                quote! { #(#attrs)* #name { #(#fs),* } }
            }
            Entry::Category { name, req_ty, .. } => quote! { #(#attrs)* #name(#req_ty) },
        }
    }

    /// Generates the variant token for the inner response enum.
    pub(crate) fn response_variant(&self) -> TokenStream {
        let attrs = self.attrs();
        match self {
            Entry::Unit { name, resp, .. }
            | Entry::Tuple { name, resp, .. }
            | Entry::Struct { name, resp, .. } => match resp {
                RespKind::Unit => quote! { #(#attrs)* #name },
                RespKind::Ty(ty) => quote! { #(#attrs)* #name(#ty) },
            },
            Entry::Category { name, resp_ty, .. } => quote! { #(#attrs)* #name(#resp_ty) },
        }
    }

    /// Generates the struct definition that goes inside `pub mod Variant { ... }`.
    pub(crate) fn struct_def(&self) -> TokenStream {
        let attrs = self.attrs();
        match self {
            Entry::Unit { name, .. } => quote! { #(#attrs)* pub struct #name; },
            Entry::Tuple { name, field, .. } => quote! { #(#attrs)* pub struct #name(pub #field); },
            Entry::Struct { name, fields, .. } => {
                let pub_fields = fields.iter().map(|(f, t)| quote! { pub #f: #t });
                quote! { #(#attrs)* pub struct #name { #(#pub_fields),* } }
            }
            #[expect(clippy::panic, reason = "Good to panic in proc-macros")]
            Entry::Category { .. } => {
                panic!("no struct_def for category")
            }
        }
    }

    /// Generates the `From<Variant::Name> for InnerReq` impl and the `TypedRequest` impl.
    ///
    /// The `From` targets `inner_req` (one hop). The `TypedRequest` impl uses chained `.into()`
    /// calls to reach `Request`, and `ExtractFromResponse` to walk `Response` down to `InnerResp`.
    /// This works uniformly at any nesting depth because the chain composes through
    /// `From`/`ExtractFromResponse` impls that each level provides for itself.
    #[expect(
        clippy::wrong_self_convention,
        reason = "Here 'from' refers to a name, not an action"
    )]
    pub(crate) fn from_and_typed_request(
        &self,
        outer_variant: &Ident,
        inner_req: &Ident,
        outer_req: &Ident,
        outer_resp: &Ident,
        outer_resp_variant: &Ident,
        inner_resp: &Ident,
    ) -> TokenStream {
        let attrs = self.attrs();
        let (name, inner_ctor) = match self {
            Entry::Unit { name, .. } => (name, quote! { #inner_req::#name }),
            Entry::Tuple { name, .. } => (name, quote! { #inner_req::#name(r.0) }),
            Entry::Struct { name, fields, .. } => {
                let fnames: Vec<_> = fields.iter().map(|(f, _)| f).collect();
                (
                    name,
                    quote! { #inner_req::#name { #(#fnames: r.#fnames),* } },
                )
            }
            #[expect(clippy::panic, reason = "Good to panic in proc-macros")]
            Entry::Category { .. } => {
                panic!("no from_and_typed_request for category")
            }
        };

        let struct_path = quote! { #outer_variant::#name };

        let from_impl = quote! {
            #(#attrs)*
            impl ::core::convert::From<#struct_path> for #inner_req {
                fn from(r: #struct_path) -> #inner_req { #inner_ctor }
            }
        };

        let resp_ty: TokenStream = match self {
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
            } => quote! { () },
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
            } => quote! { #ty },
            #[expect(
                clippy::unreachable,
                reason = "Category entries are explicitly excluded before from_and_typed_request is ever called on them"
            )]
            Entry::Category { .. } => unreachable!("Broken proc-macro logic"),
        };

        let extract_fn = match self {
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
            } => quote! {
                fn extract_response(_: crate::async_request::Response) -> #resp_ty {}
            },
            _ => quote! {
                fn extract_response(resp: crate::async_request::Response) -> #resp_ty {
                    let outer = <#outer_resp as crate::async_request::ExtractFromResponse>::extract_from_response(resp);
                    let #outer_resp::#outer_resp_variant(inner) = outer else { unreachable!() };
                    let #inner_resp::#name(v) = inner else { unreachable!() };
                    v
                }
            },
        };

        let typed_req_impl = quote! {
            #(#attrs)*
            impl crate::async_request::TypedRequest for #struct_path {
                type Response = #resp_ty;

                fn into_request(self) -> crate::async_request::Request {
                    let inner: #inner_req = ::core::convert::From::from(self);
                    let outer: #outer_req = ::core::convert::From::from(inner);
                    ::core::convert::Into::into(outer)
                }

                #extract_fn
            }
        };

        quote! {
            #from_impl
            #typed_req_impl
        }
    }
}
