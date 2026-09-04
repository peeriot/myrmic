#![expect(
    clippy::needless_continue,
    reason = "originates in darling's FromDeriveInput/FromField/FromVariant derive expansion"
)]

use proc_macro2::Ident;

pub(crate) type KeyData = darling::ast::Data<KeyVariant, KeyField>;

#[derive(Debug, darling::FromDeriveInput)]
#[darling(attributes(key), supports(struct_named, enum_tuple, enum_unit))]
pub(crate) struct DeriveInput {
    pub ident: Ident,
    pub generics: syn::Generics,
    pub data: KeyData,
    pub root: Option<syn::Path>,
}

#[derive(Clone, Debug, darling::FromField)]
#[darling(attributes(key))]
pub(crate) struct KeyField {
    pub ident: Option<Ident>,
    pub ty: syn::Type,
    #[darling(default)]
    pub raw: bool,
}

#[derive(Clone, Debug, darling::FromVariant)]
#[darling(attributes(key))]
pub(crate) struct KeyVariant {
    pub ident: Ident,
    pub discriminant: Option<syn::Expr>,
    pub fields: darling::ast::Fields<KeyField>,
}
