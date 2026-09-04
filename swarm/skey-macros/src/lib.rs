//! This derive macro implements the `StoreKey` trait found within the `skey` crate.
//! This crate shouldn't be used directly, but rather through `skey` (it has a "derive" feature).
//!
//! The implementation effectively just defers to `StoreKey::encode/decode` for each field.

mod ast;
mod codegen;
mod keys;

#[proc_macro_derive(StoreKey, attributes(key))]
pub fn key_derive(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = syn::parse_macro_input!(input as syn::DeriveInput);
    let input: ast::DeriveInput = match darling::FromDeriveInput::from_derive_input(&input) {
        Ok(parsed) => parsed,
        Err(err) => {
            return err.write_errors().into();
        }
    };

    let root = input
        .root
        .as_ref()
        .map(|path| quote::quote!(#path))
        .unwrap_or(quote::quote! {
            ::skey
        });

    codegen::expand(&root, input).into()
}

/// Defines typed key builders from a compact DSL.
///
/// ```rust
/// use skey::keys;
///
/// keys! {
///     alias scope("@", namespace: str, "*", database: str);
///     key entity(scope, "entity", id: u64);
/// }
///
/// let key = Key::entity()
///     .namespace("acme")
///     .database("inventory")
///     .id(42);
/// ```
#[proc_macro]
pub fn keys(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = syn::parse_macro_input!(input as keys::TreeDsl);
    match keys::expand(input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}
