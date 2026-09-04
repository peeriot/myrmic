//! JSON-Schema → Rust type generation via [`typify`](typify_impl).
//!
//! The bridge's `types` field is a JSON Schema document; every entry under its
//! `definitions` becomes a named Rust type. We ask `typify` to additionally
//! derive `myrmic_sdk::Message` on each type so the generated client can encode
//! them as command payloads.
//!
//! `typify` emits code anchored at the crate root — `::std`, `::serde`,
//! `::serde_json`, `::chrono`, `::uuid` — plus a few bare `alloc`-prelude names.
//! A cell is `#![no_std]` and shouldn't need those crates as direct deps, so
//! [`ExportsRewriter`] re-parses typify's output with `syn` and rewrites every
//! **absolute** path to resolve through `myrmic_sdk::codegen::exports` (which
//! re-exports exactly those crates). Using `syn` — rather than walking raw
//! tokens — means `leading_colon` tells us unambiguously which `::` anchors a
//! path, so there is no crate-name whitelist and no anchor guesswork.

use std::collections::BTreeMap;

use proc_macro2::TokenStream as Ts;
use quote::ToTokens;
use schemars::schema::RootSchema;
use syn::visit_mut::{self, VisitMut};
use typify_impl::{MapType, TypeSpace, TypeSpaceSettings};

/// Generates Rust type definitions from `schema`, returning the token stream of
/// the definitions plus a lookup from each type's name to the tokens that refer
/// to it (used by the endpoints to name their request/response types).
pub(super) fn generate(
    root: &Ts,
    schema: RootSchema,
) -> Result<(Ts, BTreeMap<String, Ts>), String> {
    let mut settings = TypeSpaceSettings::default();
    settings
        // Every payload type must be encodable/decodable by the SDK.
        .with_derive(format!("{root}::Message"))
        // `core`/`alloc` have no `HashMap`; use a `BTreeMap` for free-form maps
        // (`::std::collections::BTreeMap` is redirected to the `alloc` one).
        .with_map_type(MapType::new("::std::collections::BTreeMap"));

    let mut type_space = TypeSpace::new(&settings);
    type_space
        .add_root_schema(schema)
        .map_err(|err| format!("invalid `types` JSON Schema: {err}"))?;

    // A string `pattern` makes typify pull in the `regress` regex engine, which
    // we don't re-export. Fail clearly rather than emit code that won't build.
    if type_space.uses_regress() {
        return Err(
            "bridge `types` use a string `pattern` (regex), which is not supported yet".to_string(),
        );
    }

    let mut rw = ExportsRewriter;

    // name -> reference tokens, so endpoints can look their types up by name.
    let idents = type_space
        .iter_types()
        .map(|t| {
            let mut ty: syn::Type =
                syn::parse2(t.ident()).expect("typify type ident should parse as a type");
            rw.visit_type_mut(&mut ty);
            (t.name(), ty.to_token_stream())
        })
        .collect();

    let mut file: syn::File = syn::parse2(type_space.to_stream())
        .map_err(|err| format!("typify output did not parse as a Rust file: {err}"))?;
    rw.visit_file_mut(&mut file);

    Ok((file.to_token_stream(), idents))
}

/// Rewrites typify's output so it is self-contained in a `#![no_std]` cell that
/// depends on `myrmic-sdk` alone:
///
/// * every absolute path (`::std`/`::serde`/…) is prefixed to resolve through
///   `::myrmic_sdk::codegen::exports` — except `::myrmic_sdk` itself;
/// * bare `String`/`Vec`/`Box` are fully qualified against the same;
/// * `#[derive(::serde::…)]` paths inside attributes are rewritten too, and a
///   `#[serde(crate = …)]` is injected after the derive so serde's own expansion
///   resolves through our re-export.
struct ExportsRewriter;

impl VisitMut for ExportsRewriter {
    fn visit_type_mut(&mut self, ty: &mut syn::Type) {
        // A qualified path `<T as ::std::…::Trait>::Assoc` carries a `position`
        // marking where the `as Trait` part ends. Reanchoring the trait path
        // (via `visit_path_mut`) prepends `::myrmic_sdk::codegen::exports`, shifting
        // every segment index — so `position` must move by the same amount or the
        // `>` lands mid-path (e.g. `<f64 as ::…::exports>::std::str::FromStr::Err`).
        if let syn::Type::Path(type_path) = ty
            && let Some(qself) = &mut type_path.qself
        {
            self.visit_type_mut(&mut qself.ty);
            let before = type_path.path.segments.len();
            self.visit_path_mut(&mut type_path.path);
            let added = type_path.path.segments.len().saturating_sub(before);
            qself.position += added;
            return;
        }
        visit_mut::visit_type_mut(self, ty);
    }

    fn visit_path_mut(&mut self, path: &mut syn::Path) {
        // Recurse first so generic arguments are rewritten before the outer.
        visit_mut::visit_path_mut(self, path);

        if path.leading_colon.is_some() {
            // Absolute path — reanchor under exports, unless it is our own crate.
            if path
                .segments
                .first()
                .is_some_and(|s| s.ident == "myrmic_sdk")
            {
                return;
            }
            let mut relative = path.clone();
            relative.leading_colon = None;
            *path = syn::parse_quote!(::myrmic_sdk::codegen::exports::#relative);
        } else if path.segments.len() == 1 {
            // Bare `alloc`-prelude names typify uses unqualified.
            let seg = &path.segments[0];
            let module = match seg.ident.to_string().as_str() {
                "String" => "string",
                "Vec" => "vec",
                "Box" => "boxed",
                _ => return,
            };
            let module = syn::Ident::new(module, seg.ident.span());
            let ty = seg.ident.clone();
            let args = seg.arguments.clone();
            *path = syn::parse_quote!(::myrmic_sdk::codegen::exports::std::#module::#ty);
            path.segments.last_mut().unwrap().arguments = args;
        }
    }

    fn visit_attribute_mut(&mut self, attr: &mut syn::Attribute) {
        if attr.path().is_ident("derive") {
            // Rewrite each derive path (e.g. `::serde::Serialize`).
            let paths = attr.parse_args_with(
                syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
            );
            if let Ok(mut paths) = paths {
                for p in paths.iter_mut() {
                    self.visit_path_mut(p);
                }
                attr.meta = syn::parse_quote!(derive(#paths));
            }
        } else if attr.path().is_ident("serde") {
            // The only path serde attrs carry is a string (`skip_serializing_if
            // = "::std::option::Option::is_none"`); rewrite it in place.
            if let syn::Meta::List(list) = &mut attr.meta {
                list.tokens = redirect_std_in_strings(std::mem::take(&mut list.tokens));
            }
        }
    }

    fn visit_item_struct_mut(&mut self, item: &mut syn::ItemStruct) {
        visit_mut::visit_item_struct_mut(self, item);
        inject_serde_crate(&mut item.attrs);
    }

    fn visit_item_enum_mut(&mut self, item: &mut syn::ItemEnum) {
        visit_mut::visit_item_enum_mut(self, item);
        inject_serde_crate(&mut item.attrs);
    }
}

/// Inserts `#[serde(crate = "::myrmic_sdk::codegen::exports::serde")]` immediately
/// after a serde-bearing `#[derive]`, so serde's derive resolves through our
/// re-export. It must follow the derive (a helper attr used before its derive
/// trips the hard-deny `legacy_derive_helpers` lint). No-op for types that do
/// not derive serde (e.g. typify's `ConversionError`).
fn inject_serde_crate(attrs: &mut Vec<syn::Attribute>) {
    let idx = attrs
        .iter()
        .position(|a| a.path().is_ident("derive") && derive_mentions_serde(a));
    let Some(idx) = idx else {
        return;
    };
    attrs.insert(
        idx + 1,
        syn::Attribute {
            pound_token: syn::token::Pound::default(),
            style: syn::AttrStyle::Outer,
            bracket_token: syn::token::Bracket::default(),
            meta: syn::parse_quote!(serde(crate = "::myrmic_sdk::codegen::exports::serde")),
        },
    );
}

/// `true` if a `#[derive(...)]` attribute includes a path with a `serde`
/// segment (checked after path rewriting, so the segment is present regardless
/// of the exports prefix).
fn derive_mentions_serde(attr: &syn::Attribute) -> bool {
    attr.parse_args_with(syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated)
        .is_ok_and(|paths| {
            paths
                .iter()
                .any(|p| p.segments.iter().any(|s| s.ident == "serde"))
        })
}

/// Rewrites `::std::…` inside string-literal tokens (serde attribute values) to
/// resolve through `exports`.
fn redirect_std_in_strings(ts: Ts) -> Ts {
    use proc_macro2::{Group, Literal, TokenTree};
    ts.into_iter()
        .map(|tt| match tt {
            TokenTree::Group(g) => {
                let mut regrouped = Group::new(g.delimiter(), redirect_std_in_strings(g.stream()));
                regrouped.set_span(g.span());
                TokenTree::Group(regrouped)
            }
            TokenTree::Literal(lit) => match syn::Lit::new(lit.clone()) {
                syn::Lit::Str(s) if s.value().contains("::std::") => {
                    let v = s
                        .value()
                        .replace("::std::", "::myrmic_sdk::codegen::exports::std::");
                    TokenTree::Literal(Literal::string(&v))
                }
                _ => TokenTree::Literal(lit),
            },
            other => other,
        })
        .collect()
}
