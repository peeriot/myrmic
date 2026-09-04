use super::*;

/// Encode with the JSON default (no `--raw`) and return the wire bytes as a
/// string, since the JSON path always produces valid UTF-8.
fn json(payload: &str) -> String {
    String::from_utf8(encode(payload.to_owned(), false).unwrap()).unwrap()
}

// ── JSON default ──────────────────────────────────────────────────────

#[test]
fn valid_json_object_is_reserialised() {
    assert_eq!(json(r#"{"name": "ada"}"#), r#"{"name":"ada"}"#);
}

#[test]
fn valid_json_array_passes_through() {
    assert_eq!(json("[1, 2, 3]"), "[1,2,3]");
}

#[test]
fn bare_number_is_json_number() {
    assert_eq!(json("42"), "42");
}

#[test]
fn bare_bool_is_json_bool() {
    assert_eq!(json("true"), "true");
}

#[test]
fn bareword_falls_back_to_json_string() {
    // Not valid JSON on its own, so it is wrapped as a JSON string.
    assert_eq!(json("jsontest"), r#""jsontest""#);
}

#[test]
fn numeric_looking_but_invalid_json_falls_back_to_string() {
    assert_eq!(json("12abc"), r#""12abc""#);
}

#[test]
fn already_quoted_string_stays_a_json_string() {
    assert_eq!(json(r#""hello""#), r#""hello""#);
}

// ── --raw (hex) ───────────────────────────────────────────────────────

#[test]
fn raw_decodes_hex() {
    assert_eq!(
        encode("deadbeef".to_owned(), true).unwrap(),
        vec![0xde, 0xad, 0xbe, 0xef]
    );
}

#[test]
fn raw_accepts_0x_prefix_and_uppercase() {
    assert_eq!(encode("0xDEAD".to_owned(), true).unwrap(), vec![0xde, 0xad]);
}

#[test]
fn raw_empty_is_empty() {
    assert_eq!(encode(String::new(), true).unwrap(), Vec::<u8>::new());
}

#[test]
fn raw_rejects_odd_length() {
    assert!(encode("abc".to_owned(), true).is_err());
}

#[test]
fn raw_rejects_non_hex() {
    assert!(encode("zz".to_owned(), true).is_err());
}
