use std::collections::BTreeMap;

use heck::ToUpperCamelCase;
use proc_macro2::{Ident, Span, TokenStream};
use quote::format_ident;
use syn::spanned::Spanned;

use super::model::{KeyModel, Segment, SegmentKind, SegmentSpec};

/// Builds decode statements for the current builder state.
/// It decodes only the prefix represented by `n_fields` so partial builders can decode safely.
pub(super) fn decode_steps(flat: &[Segment], n_fields: usize, borrowed: bool) -> Vec<TokenStream> {
    let mut out = Vec::new();
    let mut filled = 0usize;

    for seg in flat {
        match seg {
            Segment::Literal(lit) => {
                if filled <= n_fields {
                    let bytes = lit.value().into_bytes();
                    out.push(quote::quote_spanned! { lit.span()=>
                        #(
                            ::skey::expect(&#bytes, decoder)?;
                        )*
                    });
                }
            }
            Segment::Field { name, kind } => {
                if filled < n_fields {
                    let ty = kind_to_storage_ty(kind, borrowed);
                    out.push(quote::quote_spanned! { name.span()=>
                        let #name: #ty = ::skey::StoreKey::decode_from(decoder)?;
                    });
                    filled += 1;
                } else {
                    break;
                }
            }
        }
    }

    out
}

/// Expands alias references into concrete segments.
/// Recursive expansion with a stack detects cycles and reports clear compile-time errors.
pub(super) fn resolve_aliases(
    segments: &[SegmentSpec],
    aliases: &BTreeMap<String, (Ident, Vec<SegmentSpec>)>,
    stack: &mut Vec<String>,
) -> syn::Result<Vec<Segment>> {
    let mut out = Vec::new();

    for segment in segments {
        match segment {
            SegmentSpec::Literal(lit) => out.push(Segment::Literal(lit.clone())),
            SegmentSpec::Field { name, kind } => out.push(Segment::Field {
                name: name.clone(),
                kind: kind.clone(),
            }),
            SegmentSpec::Ref(name) => {
                let key = name.to_string();
                if stack.contains(&key) {
                    return Err(syn::Error::new(name.span(), "alias cycle detected"));
                }
                let Some((_, parts)) = aliases.get(&key) else {
                    return Err(syn::Error::new(name.span(), "unknown alias reference"));
                };

                stack.push(key.clone());
                let segments = resolve_aliases(parts, aliases, stack)?;
                stack.pop();

                out.extend(segments);
            }
        }
    }

    Ok(out)
}

/// Extracts field segments in declaration order and rejects duplicate names.
/// Unique field names are required so generated builders have unambiguous members.
pub(super) fn collect_fields(flat: &[Segment]) -> syn::Result<Vec<(Ident, SegmentKind)>> {
    let mut seen = BTreeMap::<String, Span>::new();
    let mut fields = Vec::new();

    for seg in flat {
        if let Segment::Field { name, kind } = seg {
            if let Some(prev) = seen.insert(name.to_string(), name.span()) {
                let mut err = syn::Error::new(
                    name.span(),
                    format!("duplicate field segment `{}` in expanded key", name),
                );
                err.combine(syn::Error::new(prev, "first declared here"));
                return Err(err);
            }
            fields.push((name.clone(), kind.clone()));
        }
    }

    Ok(fields)
}

/// Returns the first `count` fields of a key model.
/// Builder states use this to materialize just the fields known at each step.
pub(super) fn fields_prefix(
    fields: &[(Ident, SegmentKind)],
    count: usize,
) -> Vec<(Ident, SegmentKind)> {
    fields.iter().take(count).cloned().collect()
}

/// Checks whether a field slice matches the expected alias field sequence.
/// This gates generation of alias helper methods to valid positions only.
pub(super) fn field_slice_matches(
    fields: &[(Ident, SegmentKind)],
    start: usize,
    expected: &[(Ident, SegmentKind)],
) -> bool {
    if start + expected.len() > fields.len() {
        return false;
    }

    fields[start..start + expected.len()]
        .iter()
        .zip(expected.iter())
        .all(|((left_name, left_kind), (right_name, right_kind))| {
            left_name == right_name && left_kind == right_kind
        })
}

/// Computes the generated builder type name for a key state index.
/// Stable naming keeps emitted API predictable and avoids collisions.
pub(super) fn state_ident(key_name: &Ident, index: usize) -> Ident {
    let base = format!("{}Builder", key_name.to_string().to_upper_camel_case());
    if index == 0 {
        format_ident!("{base}")
    } else {
        format_ident!("{base}{index}")
    }
}

/// Finds keys that can be reached as strict prefix extensions of `model`.
/// This enables cross-key transition helpers while enforcing prefix-field compatibility.
pub(super) fn child_keys<'a>(
    model: &KeyModel,
    all: &'a [KeyModel],
) -> syn::Result<Vec<&'a KeyModel>> {
    let mut children = Vec::new();

    for other in all {
        if model.name == other.name {
            continue;
        }

        if is_prefix(&model.segments, &other.segments) {
            let prefix_fields = model.fields.len();
            let shared = count_shared_prefix_fields(&model.segments, &other.segments);
            if shared != prefix_fields {
                let mismatch_idx = model
                    .segments
                    .iter()
                    .zip(other.segments.iter())
                    .position(|(left, right)| left != right)
                    .unwrap_or_default();
                let span = other.segments[mismatch_idx].span();
                return Err(syn::Error::new(
                    span,
                    "prefix keys must share identical field sequence",
                ));
            }

            children.push(other);
        }
    }

    Ok(children)
}

/// Computes the child builder index after walking beyond the parent's full segment prefix.
/// This identifies where cross-key transition methods should land.
pub(super) fn child_target_state_index(parent: &KeyModel, child: &KeyModel) -> syn::Result<usize> {
    let mut pos = parent.segments.len();
    while pos < child.segments.len() {
        match &child.segments[pos] {
            Segment::Literal(_) => pos += 1,
            Segment::Field { .. } => break,
        }
    }

    let idx = count_fields_before(&child.segments, pos);
    if idx < parent.fields.len() {
        let span = child
            .segments
            .get(pos)
            .map_or_else(|| child.name.span(), Segment::span);
        return Err(syn::Error::new(span, "invalid prefix transition target"));
    }

    Ok(idx)
}

/// Counts shared leading field segments between two flattened segment lists.
/// This supports validation that prefix relationships preserve field ordering.
fn count_shared_prefix_fields(a: &[Segment], b: &[Segment]) -> usize {
    let mut count = 0;
    for (sa, sb) in a.iter().zip(b.iter()) {
        if sa != sb {
            break;
        }
        if matches!(sa, Segment::Field { .. }) {
            count += 1;
        }
    }
    count
}

/// Counts field segments before a segment position.
/// The result maps a segment index into a builder state index.
fn count_fields_before(flat: &[Segment], until: usize) -> usize {
    flat.iter()
        .take(until)
        .filter(|seg| matches!(seg, Segment::Field { .. }))
        .count()
}

/// Returns whether `prefix` is a strict segment prefix of `full` (not an exact match).
/// Cross-key transition generation depends on rejecting complete key matches here.
fn is_prefix(prefix: &[Segment], full: &[Segment]) -> bool {
    prefix.len() < full.len() && full.starts_with(prefix)
}

/// Builds encode statements for the current number of assigned fields.
/// This ensures partial builder states encode only the prefix they represent.
pub(super) fn encode_steps(flat: &[Segment], fields_set: usize) -> Vec<TokenStream> {
    let mut steps = Vec::new();
    let mut seen = 0usize;

    for seg in flat {
        let segment_steps = match seg {
            Segment::Literal(lit) => {
                let bytes = lit.value().into_bytes();
                bytes
                    .into_iter()
                    .map(|byte| quote::quote_spanned!(lit.span()=> ::skey::StoreKey::encode_into(&#byte, encoder)?;))
                    .collect::<Vec<_>>()
            }
            Segment::Field { name, kind: _ } => {
                if seen >= fields_set {
                    break;
                }
                seen += 1;
                vec![
                    quote::quote_spanned!(name.span()=> ::skey::StoreKey::encode_into(&self.#name, encoder)?;),
                ]
            }
        };

        steps.extend(segment_steps);
    }

    steps
}

/// Maps a segment kind to the generated builder storage type.
/// Borrowed keys store references while owned keys store owned container types.
pub(super) fn kind_to_storage_ty(kind: &SegmentKind, borrowed: bool) -> TokenStream {
    match kind {
        SegmentKind::Str if borrowed => quote::quote!(&'a str),
        SegmentKind::Str => quote::quote!(::std::string::String),
        SegmentKind::Bytes if borrowed => quote::quote!(&'a [u8]),
        SegmentKind::Bytes => quote::quote!(::std::vec::Vec<u8>),
        SegmentKind::Type { ty, .. } => quote::quote!(#ty),
    }
}

/// Maps a segment kind to method argument type for generated setters.
/// `Into` is used for owned forms to keep call sites ergonomic.
pub(super) fn kind_to_arg_ty(kind: &SegmentKind, borrowed: bool) -> TokenStream {
    match kind {
        SegmentKind::Str if borrowed => quote::quote!(&'a str),
        SegmentKind::Str => quote::quote!(impl ::std::convert::Into<::std::string::String>),
        SegmentKind::Bytes if borrowed => quote::quote!(&'a [u8]),
        SegmentKind::Bytes => quote::quote!(impl ::std::convert::Into<::std::vec::Vec<u8>>),
        SegmentKind::Type { ty, .. } => {
            quote::quote_spanned!(ty.span()=> impl ::std::convert::Into<#ty>)
        }
    }
}

/// Produces the assignment expression for a setter argument.
/// Non-builtin segment types are normalized through `Into` for consistent conversions.
pub(super) fn kind_to_assign_expr(kind: &SegmentKind, value: TokenStream) -> TokenStream {
    match kind {
        SegmentKind::Str | SegmentKind::Bytes => value,
        SegmentKind::Type { ty, .. } => quote::quote_spanned!(ty.span()=> (#value).into()),
    }
}

/// Computes the terminal builder type token for an alias key.
/// Alias helper methods use this so accepted argument types match generated key lifetimes.
pub(super) fn alias_terminal_ty(model: &KeyModel) -> TokenStream {
    let term_ident = state_ident(&model.name, model.fields.len());
    if model.has_borrowed_fields {
        quote::quote!(#term_ident<'a>)
    } else {
        quote::quote!(#term_ident)
    }
}
