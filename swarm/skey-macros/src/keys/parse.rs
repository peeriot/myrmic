use proc_macro2::Ident;
use quote::ToTokens;
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;

use super::model::{
    AliasDef, KeyAttrs, KeyDef, SegmentKind, SegmentSpec, TreeDsl, TreeItem, TypeDef,
};

#[derive(Copy, Clone, Eq, PartialEq)]
enum DeclKind {
    Alias,
    Key,
    Type,
}

impl Parse for TreeDsl {
    /// Parses the top-level DSL into a typed intermediate tree.
    /// This gives codegen a validated structure instead of raw tokens.
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let items = parse_items(input)?;

        Ok(Self { items })
    }
}

/// Parses nested items and inline `{ ... }` groups.
/// Recursion here lets the DSL support scoped grouping without changing semantics.
fn parse_items(input: ParseStream<'_>) -> syn::Result<Vec<TreeItem>> {
    let mut items = Vec::new();

    while !input.is_empty() {
        let attrs = input.call(syn::Attribute::parse_outer)?;

        if input.peek(syn::token::Brace) {
            if let Some(attr) = attrs.first() {
                return Err(syn::Error::new(
                    attr.span(),
                    "attributes are only supported on `key` declarations or `type` aliases",
                ));
            }
            let content;
            syn::braced!(content in input);
            let children = parse_items(&content)?;
            items.extend(children);
            continue;
        }

        let decl_kind = if input.peek(syn::Token![use]) {
            input.parse::<syn::Token![use]>()?;
            DeclKind::Type
        } else {
            let head: Ident = input.parse()?;
            if head == "alias" {
                DeclKind::Alias
            } else if head == "key" {
                DeclKind::Key
            } else {
                return Err(syn::Error::new(
                    head.span(),
                    "expected `alias`, `key`, `type`, or `{ ... }`",
                ));
            }
        };
        let item_attrs = parse_attrs(&attrs, decl_kind)?;

        let item = if decl_kind == DeclKind::Type {
            let ty: syn::Type = input.parse()?;
            let name = if input.peek(syn::Token![as]) {
                input.parse::<syn::Token![as]>()?;
                let name: Ident = input.parse()?;
                name
            } else {
                match &ty {
                    syn::Type::Path(path) => {
                        if path.qself.is_some() {
                            return Err(syn::Error::new_spanned(
                                &ty,
                                "qualified type paths require an explicit alias name; use `as <name>`",
                            ));
                        }

                        path.path
                            .segments
                            .last()
                            .map(|segment| segment.ident.clone())
                            .ok_or_else(|| {
                                syn::Error::new_spanned(
                                    &ty,
                                    "type path is missing an identifier; use `as <name>`",
                                )
                            })?
                    }
                    _ => {
                        return Err(syn::Error::new_spanned(
                            &ty,
                            "type alias name cannot be inferred from this type; use `as <name>`",
                        ));
                    }
                }
            };

            input.parse::<syn::Token![;]>()?;

            TreeItem::Type(TypeDef {
                name,
                ty,
                no_copy: item_attrs.no_copy,
            })
        } else {
            let name: Ident = input.parse()?;

            let segments = parse_segments(input)?;
            input.parse::<syn::Token![;]>()?;

            if decl_kind == DeclKind::Alias {
                TreeItem::Alias(AliasDef { name, segments })
            } else {
                TreeItem::Key(KeyDef {
                    name,
                    docs: item_attrs.docs,
                    segments,
                })
            }
        };

        items.push(item);
    }

    Ok(items)
}

/// Validates and collects attributes allowed on declarations.
/// Keys support docs, and types support `#[no_copy]`.
fn parse_attrs(attrs: &[syn::Attribute], decl_kind: DeclKind) -> syn::Result<KeyAttrs> {
    let mut parsed = KeyAttrs::default();

    for attr in attrs {
        let msg = match (decl_kind, attr.path().get_ident()) {
            (DeclKind::Key, Some(ident)) if ident == "doc" => {
                parsed.docs.push(attr.clone());
                continue;
            }
            (DeclKind::Type, Some(ident)) if ident == "no_copy" => {
                parsed.no_copy = true;
                continue;
            }
            (DeclKind::Key, _) => "unsupported key attribute; expected a doc comment",
            (DeclKind::Type, _) => "unsupported type attribute; expected `#[no_copy]`",
            (DeclKind::Alias, _) => "attributes are not supported on `alias` declarations",
        };

        return Err(syn::Error::new(attr.span(), msg));
    }

    Ok(parsed)
}

/// Parses the segment list inside `( ... )` for an alias or key.
/// It normalizes typed fields into `SegmentKind` so later stages can stay simple.
fn parse_segments(input: ParseStream<'_>) -> syn::Result<Vec<SegmentSpec>> {
    let args;
    syn::parenthesized!(args in input);

    let mut segments = Vec::new();
    while !args.is_empty() {
        if args.peek(syn::LitStr) {
            segments.push(SegmentSpec::Literal(args.parse()?));
        } else {
            let ident: Ident = args.parse()?;
            if args.peek(syn::Token![:]) {
                args.parse::<syn::Token![:]>()?;
                let kind_ty: syn::Type = args.parse()?;
                let kind_name = kind_ty.to_token_stream().to_string().replace(' ', "");
                let kind = match kind_name.as_str() {
                    "str" => SegmentKind::Str,
                    "[u8]" => SegmentKind::Bytes,
                    _ => SegmentKind::Type {
                        ty: Box::new(kind_ty),
                        repr: kind_name,
                    },
                };
                segments.push(SegmentSpec::Field { name: ident, kind });
            } else {
                segments.push(SegmentSpec::Ref(ident));
            }
        }

        if !args.is_empty() {
            args.parse::<syn::Token![,]>()?;
        }
    }

    Ok(segments)
}
