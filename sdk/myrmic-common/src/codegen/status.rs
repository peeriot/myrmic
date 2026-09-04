//! Maps HTTP status codes to the `<Endpoint>Reply` enum variant names, shared by
//! codegen and the runtime's reply builder.

use heck::ToUpperCamelCase;

/// The `<Endpoint>Reply` enum variant name for a listed `response:` status code.
///
/// Uses the status's canonical reason phrase (`200` -> `Ok`, `404` -> `NotFound`,
/// `207` -> `MultiStatus`), falling back to `Status<code>` for codes without one.
/// Both the generated enum and the runtime's `build_reply` call this, so the
/// externally-tagged serde name always matches on each side.
///
/// Errors on codes outside the valid HTTP range, so an invalid `response:` key is
/// rejected at import time rather than producing a bogus variant.
pub fn status_variant_name(code: u16) -> Result<String, String> {
    let status = http::StatusCode::from_u16(code)
        .map_err(|_| format!("invalid HTTP status code `{code}` in `response`"))?;
    Ok(match status.canonical_reason() {
        Some(reason) => reason.to_upper_camel_case(),
        None => format!("Status{code}"),
    })
}

#[cfg(test)]
mod tests {
    use super::status_variant_name;

    #[test]
    fn canonical_reasons_become_camel_idents() {
        assert_eq!(status_variant_name(200).unwrap(), "Ok");
        assert_eq!(status_variant_name(201).unwrap(), "Created");
        assert_eq!(status_variant_name(204).unwrap(), "NoContent");
        assert_eq!(status_variant_name(207).unwrap(), "MultiStatus");
        assert_eq!(status_variant_name(404).unwrap(), "NotFound");
        // `heck` title-cases each word, so the single-letter word in "I'm a teapot"
        // yields consecutive capitals — ugly but deterministic and a valid ident.
        assert_eq!(status_variant_name(418).unwrap(), "IMATeapot");
        assert_eq!(status_variant_name(500).unwrap(), "InternalServerError");
    }

    #[test]
    fn codes_without_a_reason_fall_back_to_status_code() {
        assert_eq!(status_variant_name(299).unwrap(), "Status299");
        assert_eq!(status_variant_name(999).unwrap(), "Status999");
    }

    #[test]
    fn out_of_range_codes_error() {
        assert!(status_variant_name(0).is_err());
        assert!(status_variant_name(99).is_err());
        assert!(status_variant_name(1000).is_err());
    }
}
