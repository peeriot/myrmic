//! `#[derive(Message)]` — binds a wire codec to a message type.
//!
//! Reads an optional `#[codec(Path)]` helper attribute naming a `Codec` type
//! (built-in `Json`/`Postcard` or a user impl) and generates `Decoder` +
//! `Encoder` impls that delegate to it — `BorrowDecoder` in place of `Decoder`
//! for a type with lifetime parameters. When the attribute is omitted the
//! codec defaults to `Json`. The type is expected to also derive
//! `serde::{Serialize, Deserialize}`.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{DeriveInput, Path, parse_quote, parse2};

pub(crate) fn derive_message(item: TokenStream, root: &TokenStream) -> TokenStream {
    let input: DeriveInput = match parse2(item) {
        Ok(input) => input,
        Err(err) => return err.to_compile_error(),
    };

    let codec = match codec_attr(&input) {
        Ok(Some(path)) => quote! { #path },
        Ok(None) => quote! { #root::Json },
        Err(err) => return err.to_compile_error(),
    };

    let ident = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    // A type with a lifetime can only be decoded borrowing from the payload.
    let decoder = if input.generics.lifetimes().next().is_some() {
        let mut generics = input.generics.clone();
        generics.params.insert(0, parse_quote!('__de));
        generics
            .make_where_clause()
            .predicates
            .push(parse_quote!(Self: #root::serde::Deserialize<'__de>));
        let (impl_generics, _, where_clause) = generics.split_for_impl();

        quote! {
            impl #impl_generics #root::BorrowDecoder<'__de> for #ident #ty_generics #where_clause {
                fn decode(bytes: &'__de [u8]) -> #root::Result<Self> {
                    <#codec as #root::Codec>::decode(bytes)
                }
            }
        }
    } else {
        quote! {
            impl #impl_generics #root::Decoder for #ident #ty_generics #where_clause {
                fn from_bytes(bytes: #root::Bytes) -> #root::Result<Self> {
                    <#codec as #root::Codec>::decode(&bytes)
                }
            }
        }
    };

    quote! {
        #decoder

        impl #impl_generics #root::Encoder for #ident #ty_generics #where_clause {
            fn to_bytes(&self) -> #root::Result<#root::Bytes> {
                <#codec as #root::Codec>::encode(self)
            }
        }
    }
}

/// Extracts the optional `#[codec(Path)]` attribute — at most once. `None`
/// means the caller should fall back to the default codec.
fn codec_attr(input: &DeriveInput) -> syn::Result<Option<Path>> {
    let mut codec = None;
    for attr in &input.attrs {
        if attr.path().is_ident("codec") {
            if codec.is_some() {
                return Err(syn::Error::new_spanned(attr, "duplicate `#[codec(...)]`"));
            }
            codec = Some(attr.parse_args::<Path>()?);
        }
    }
    Ok(codec)
}
