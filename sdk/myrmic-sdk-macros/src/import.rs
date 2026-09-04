//! The `import!` macro: reads bridge spec files and generates a typed client
//! (plus its payload/event types) for each.
//!
//! The actual code generation lives in [`myrmic_common::codegen::generate`]; this
//! module only handles resolving the file paths, parsing them into the bridge
//! model, and mapping any generation error back onto the invocation span.

use std::path::PathBuf;

use proc_macro2::{Span, TokenStream as Ts};
use quote::quote;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;

use myrmic_common::codegen::generate;

use crate::inputs::ImportInput;

pub(crate) fn import_misc(input: Ts, root: &Ts) -> Ts {
    import_inner(input, root).unwrap_or_else(|err| err.to_compile_error())
}

fn import_inner(input: Ts, root: &Ts) -> Result<Ts, syn::Error> {
    let paths: Punctuated<syn::LitStr, syn::Token![,]> =
        syn::parse::Parser::parse2(Punctuated::parse_terminated, input)?;

    if paths.is_empty() {
        return Err(syn::Error::new(
            paths.span(),
            "import! requires at least one path",
        ));
    }

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .map_err(|_| syn::Error::new(Span::call_site(), "CARGO_MANIFEST_DIR not set"))?;
    let crate_root = PathBuf::from(&manifest_dir);

    let mut out = Ts::new();
    for path_lit in &paths {
        let path = crate_root.join(path_lit.value());

        let input = crate::inputs::parse_from_file::<ImportInput>(&path)
            .map_err(|err| syn::Error::new(path_lit.span(), err))?;

        let tokens = match input {
            ImportInput::Mqtt(v) => generate::mqtt_bridge(root, v),
            ImportInput::Http(v) => generate::http_bridge(root, v),
        }
        .map_err(|err| syn::Error::new(path_lit.span(), err))?;

        // Register the spec as a build input. A proc-macro's plain file read is
        // invisible to cargo, so without this an edited spec wouldn't re-trigger
        // codegen and the generated client would go stale. `include_bytes!`
        // records the path in the crate's dep-info; the anonymous const is
        // unused (and stripped), we only want the dependency edge.
        let path_str = path
            .to_str()
            .ok_or_else(|| syn::Error::new(path_lit.span(), "spec path is not valid UTF-8"))?;
        out.extend(quote! {
            const _: &[u8] = include_bytes!(#path_str);
        });

        out.extend(tokens);
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_include_bytes_so_spec_edits_trigger_rebuild() {
        // A proc-macro that reads an external file must reference it via
        // `include_bytes!` (or `tracked_path`) or cargo won't rebuild when the
        // spec changes — leaving a stale generated client.
        let out = import_inner(
            quote! { "tests/data/http-bridge.yml" },
            &quote! { ::myrmic_sdk },
        )
        .expect("fixture should generate");

        // The whole output (tracking item included) must be valid Rust.
        syn::parse2::<syn::File>(out.clone()).expect("import! output should be a valid Rust file");

        let text = out.to_string();

        assert!(
            text.contains("include_bytes"),
            "generated output must include_bytes! the spec for rebuild tracking:\n{text}"
        );
        assert!(
            text.contains("http-bridge.yml"),
            "include_bytes! should reference the resolved spec path:\n{text}"
        );
    }
}
