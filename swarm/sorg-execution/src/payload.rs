use std::collections::HashMap;

use serde::de::DeserializeOwned;
use serde_json::Value;
use sorg_common::{BodyTemplate, TemplateSegment, TemplateSegments, custom_err};

macro_rules! impl_extracts {
    ($( ($fn:ident, $variant:ident, $ty:ty, $label:literal) ),* $(,)?) => {
        const _: () = {
            impl Val {
                fn label(&self) -> &'static str {
                    match self {
                        $( Val::$variant(_) => $label, )*
                    }
                }

                $(
                    pub fn $fn(self) -> Result<$ty, String> {
                        match self {
                            Val::$variant(v) => Ok(v),
                            other => Err(format!(
                                "expected `{}`, found `{}`",
                                $label,
                                other.label(),
                            )),
                        }
                    }
                )*
            }
        };

    };
}

impl_extracts! {
    (into_string, String, String,             "string"),
    (into_bytes,  Bytes,  Vec<u8>,            "bytes"),
    (into_json,   Json,   serde_json::Value,  "json"),
    (into_bool,   Bool,   bool,               "bool"),
    (into_u8,     U8,     u8,                 "u8"),
    (into_u16,    U16,    u16,                "u16"),
    (into_u32,    U32,    u32,                "u32"),
    (into_u64,    U64,    u64,                "u64"),
    (into_i8,     I8,     i8,                 "i8"),
    (into_i16,    I16,    i16,                "i16"),
    (into_i32,    I32,    i32,                "i32"),
    (into_i64,    I64,    i64,                "i64"),
    (into_f32,    F32,    f32,                "f32"),
    (into_f64,    F64,    f64,                "f64"),
}

/// Look up a decoded field by its placeholder name. `decode_vals` guarantees every
/// marker's name is present, so a miss here is an internal invariant violation.
pub(crate) fn get_val(vals: &HashMap<String, Val>, name: &str) -> crate::Result<Val> {
    match vals.get(name) {
        Some(val) => Ok(val.clone()),
        None => Err(custom_err!(
            "internal error: field `{}` was not decoded",
            name
        ))?,
    }
}

pub(crate) async fn resolve_segments_as_string(
    db: &db_client::v1::Client,
    segments: TemplateSegments,
    vals: &HashMap<String, Val>,
) -> crate::Result<String> {
    let mut out = String::new();
    for seg in segments.into_segments() {
        let seg = resolve_segment_as_string(db, seg, vals).await?;
        out.push_str(&seg);
    }
    Ok(out)
}

#[allow(clippy::too_many_lines)]
pub(crate) async fn resolve_segment_as_string(
    db: &db_client::v1::Client,
    segment: TemplateSegment,
    vals: &HashMap<String, Val>,
) -> crate::Result<String> {
    let value = match segment {
        TemplateSegment::Raw(value) => value,
        TemplateSegment::String(name) => get_val(vals, &name)?
            .into_string()
            .map_err(|err| custom_err!("{}", err))?,
        TemplateSegment::Json(name) => {
            let value = get_val(vals, &name)?
                .into_json()
                .map_err(|err| custom_err!("{}", err))?;
            format!("{}", value)
        }
        TemplateSegment::Bool(name) => {
            let value = get_val(vals, &name)?
                .into_bool()
                .map_err(|err| custom_err!("{}", err))?;
            format!("{}", value)
        }
        TemplateSegment::U8(name) => {
            let value = get_val(vals, &name)?
                .into_u8()
                .map_err(|err| custom_err!("{}", err))?;
            format!("{}", value)
        }
        TemplateSegment::U16(name) => {
            let value = get_val(vals, &name)?
                .into_u16()
                .map_err(|err| custom_err!("{}", err))?;
            format!("{}", value)
        }
        TemplateSegment::U32(name) => {
            let value = get_val(vals, &name)?
                .into_u32()
                .map_err(|err| custom_err!("{}", err))?;
            format!("{}", value)
        }
        TemplateSegment::U64(name) => {
            let value = get_val(vals, &name)?
                .into_u64()
                .map_err(|err| custom_err!("{}", err))?;
            format!("{}", value)
        }
        TemplateSegment::I8(name) => {
            let value = get_val(vals, &name)?
                .into_i8()
                .map_err(|err| custom_err!("{}", err))?;
            format!("{}", value)
        }
        TemplateSegment::I16(name) => {
            let value = get_val(vals, &name)?
                .into_i16()
                .map_err(|err| custom_err!("{}", err))?;
            format!("{}", value)
        }
        TemplateSegment::I32(name) => {
            let value = get_val(vals, &name)?
                .into_i32()
                .map_err(|err| custom_err!("{}", err))?;
            format!("{}", value)
        }
        TemplateSegment::I64(name) => {
            let value = get_val(vals, &name)?
                .into_i64()
                .map_err(|err| custom_err!("{}", err))?;
            format!("{}", value)
        }
        TemplateSegment::F32(name) => {
            let value = get_val(vals, &name)?
                .into_f32()
                .map_err(|err| custom_err!("{}", err))?;
            format!("{}", value)
        }
        TemplateSegment::F64(name) => {
            let value = get_val(vals, &name)?
                .into_f64()
                .map_err(|err| custom_err!("{}", err))?;
            format!("{}", value)
        }
        TemplateSegment::Db(seg) => {
            let (ns, database, schema, key) = seg.into_tuple();

            tracing::debug!(
                "attempting to resolve segment as a db key: {}/{}/{} @ key[{}]",
                ns,
                database,
                schema,
                key
            );

            let scope =
                db_client::v1::models::Scope::new(ns.clone(), database.clone(), schema.clone());

            let value = db
                .read_tx_in(scope, {
                    let ns = ns.clone();
                    let database = database.clone();
                    let schema = schema.clone();
                    let key = key.clone();

                    async move |c, tx_id| {
                        use db_client::v1::models;

                        c.send(models::key_get::Request {
                            id: tx_id,
                            op: models::key_get::Op {
                                scope: models::Scope::new(ns, database, schema),
                                key,
                            },
                        })
                        .await
                    }
                })
                .await
                .map_err(|err| custom_err!("unable to talk to db: {}", err))?
                .map_err(|err| custom_err!("unable to fetch key: {}", err.message))?
                .value;

            let Some(value) = value else {
                return Err(custom_err!(
                    "key was not found: {}/{}/{} @ key[{}]",
                    ns,
                    database,
                    schema,
                    key
                ))?;
            };

            String::from_utf8(value).map_err(|_| {
                custom_err!(
                    "key was not valid utf8: {}/{}/{} @ key[{}]",
                    ns,
                    database,
                    schema,
                    key
                )
            })?
        }
    };

    Ok(value)
}

/// The reserved JSON key the HTTP request body always travels under. The
/// generated client names the body field `body` regardless of the placeholder's
/// (schema type) name, so the bridge decodes and resolves it by this fixed key.
/// A request placeholder named `body` is rejected at codegen, so there is no
/// collision.
pub(crate) const HTTP_BODY_FIELD: &str = "body";

/// A single named placeholder expected in the payload: its `name` (the JSON key)
/// and the `kind` its value must coerce to.
#[derive(Debug)]
pub(crate) struct Marker {
    name: String,
    kind: Kind,
}

#[derive(Debug, Clone, Copy)]
enum Kind {
    String,
    Bytes,
    Json,
    Bool,
    U8,
    U16,
    U32,
    U64,
    I8,
    I16,
    I32,
    I64,
    F32,
    F64,
}

impl Marker {
    pub fn collect(segs: &TemplateSegments) -> Vec<Marker> {
        segs.iter().filter_map(Self::from_segment).collect()
    }

    fn from_segment(seg: &TemplateSegment) -> Option<Self> {
        let (kind, name) = match seg {
            TemplateSegment::String(name) => (Kind::String, name),
            TemplateSegment::Json(name) => (Kind::Json, name),
            TemplateSegment::Bool(name) => (Kind::Bool, name),
            TemplateSegment::U8(name) => (Kind::U8, name),
            TemplateSegment::U16(name) => (Kind::U16, name),
            TemplateSegment::U32(name) => (Kind::U32, name),
            TemplateSegment::U64(name) => (Kind::U64, name),
            TemplateSegment::I8(name) => (Kind::I8, name),
            TemplateSegment::I16(name) => (Kind::I16, name),
            TemplateSegment::I32(name) => (Kind::I32, name),
            TemplateSegment::I64(name) => (Kind::I64, name),
            TemplateSegment::F32(name) => (Kind::F32, name),
            TemplateSegment::F64(name) => (Kind::F64, name),
            TemplateSegment::Raw(_) | TemplateSegment::Db(_) => return None,
        };
        Some(Self {
            name: name.clone(),
            kind,
        })
    }

    pub fn from_body_template(seg: &BodyTemplate) -> Self {
        let (kind, name) = match seg {
            BodyTemplate::String(name) => (Kind::String, name),
            BodyTemplate::Json(name) => (Kind::Json, name),
            BodyTemplate::Bytes(name) => (Kind::Bytes, name),
        };
        Self {
            name: name.clone(),
            kind,
        }
    }

    /// The HTTP request-body marker: the body's declared `kind`, but keyed under
    /// the reserved [`HTTP_BODY_FIELD`] the client always encodes it under
    /// (rather than the placeholder's name, which for HTTP is the schema type).
    pub fn http_body(seg: &BodyTemplate) -> Self {
        Self {
            name: HTTP_BODY_FIELD.to_owned(),
            ..Self::from_body_template(seg)
        }
    }
}

impl Kind {
    fn coerce(self, value: Value) -> Result<Val, String> {
        fn from<T: DeserializeOwned>(value: Value) -> Result<T, String> {
            serde_json::from_value(value).map_err(|err| err.to_string())
        }

        Ok(match self {
            Kind::Json => Val::Json(value),
            Kind::String => Val::String(from(value)?),
            Kind::Bytes => Val::Bytes(from(value)?),
            Kind::Bool => Val::Bool(from(value)?),
            Kind::U8 => Val::U8(from(value)?),
            Kind::U16 => Val::U16(from(value)?),
            Kind::U32 => Val::U32(from(value)?),
            Kind::U64 => Val::U64(from(value)?),
            Kind::I8 => Val::I8(from(value)?),
            Kind::I16 => Val::I16(from(value)?),
            Kind::I32 => Val::I32(from(value)?),
            Kind::I64 => Val::I64(from(value)?),
            Kind::F32 => Val::F32(from(value)?),
            Kind::F64 => Val::F64(from(value)?),
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) enum Val {
    String(String),
    Bytes(Vec<u8>),
    Json(serde_json::Value),
    Bool(bool),
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
}

/// Parses a command payload into its JSON object. An absent payload is an empty
/// object (valid only if nothing is expected); a non-object payload is rejected.
pub(crate) fn parse_payload_object(data: &[u8]) -> crate::Result<serde_json::Map<String, Value>> {
    if data.is_empty() {
        return Ok(serde_json::Map::new());
    }
    match serde_json::from_slice::<Value>(data)
        .map_err(|err| custom_err!("unable to decode payload as json: {}", err))?
    {
        Value::Object(obj) => Ok(obj),
        _ => Err(custom_err!("expected a json object payload"))?,
    }
}

/// Decode the JSON payload object into the values named by `markers`.
///
/// Each marker pulls its field out of `obj` and coerces it to the declared type.
/// Fields are validated eagerly so a bad payload fails before the bridge performs
/// any request/publish. Placeholders referenced more than once share a single
/// decoded value. Any field left over — not named by a placeholder — is rejected;
/// callers that carry reserved keys (e.g. the HTTP bridge's `__callback`) must
/// remove them before calling this.
pub(crate) fn decode_vals(
    markers: Vec<Marker>,
    mut obj: serde_json::Map<String, Value>,
) -> crate::Result<HashMap<String, Val>> {
    let mut vals = HashMap::with_capacity(markers.len());

    for Marker { name, kind } in markers {
        if vals.contains_key(&name) {
            continue;
        }

        tracing::debug!("attempting to read `{}` as {:?}", name, kind);

        let Some(value) = obj.remove(&name) else {
            return Err(custom_err!("missing field `{}` in payload", name))?;
        };

        let val = kind
            .coerce(value)
            .map_err(|err| custom_err!("field `{}`: {}", name, err))?;

        vals.insert(name, val);
    }

    if !obj.is_empty() {
        let unknown = obj
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        Err(custom_err!("unknown field(s) in payload: {}", unknown))?;
    }

    Ok(vals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        match value {
            serde_json::Value::Object(map) => map,
            other => panic!("test payload must be a json object, got {other}"),
        }
    }

    fn marker(name: &str, kind: Kind) -> Marker {
        Marker {
            name: name.to_owned(),
            kind,
        }
    }

    #[test]
    fn decodes_named_fields_coerced_to_their_kind() {
        let markers = vec![marker("name", Kind::String), marker("count", Kind::U32)];
        let vals = decode_vals(markers, object(json!({"name": "bob", "count": 7}))).unwrap();

        assert_eq!(
            get_val(&vals, "name").unwrap().into_string().unwrap(),
            "bob"
        );
        assert_eq!(get_val(&vals, "count").unwrap().into_u32().unwrap(), 7);
    }

    #[test]
    fn json_kind_keeps_the_nested_value_not_a_string() {
        let markers = vec![marker("body", Kind::Json)];
        let vals = decode_vals(markers, object(json!({"body": {"k": 1}}))).unwrap();

        assert_eq!(
            get_val(&vals, "body").unwrap().into_json().unwrap(),
            json!({"k": 1})
        );
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let markers = vec![marker("a", Kind::String)];
        let err = decode_vals(markers, object(json!({"a": "x", "b": "y"}))).unwrap_err();
        assert!(err.to_string().contains("unknown field"), "got: {err}");
    }

    #[test]
    fn missing_field_errors() {
        let markers = vec![marker("a", Kind::String)];
        let err = decode_vals(markers, object(json!({}))).unwrap_err();
        assert!(err.to_string().contains("missing field"), "got: {err}");
    }

    #[test]
    fn a_placeholder_used_twice_shares_one_decoded_value() {
        let markers = vec![marker("a", Kind::String), marker("a", Kind::String)];
        let vals = decode_vals(markers, object(json!({"a": "x"}))).unwrap();
        assert_eq!(vals.len(), 1);
        assert_eq!(get_val(&vals, "a").unwrap().into_string().unwrap(), "x");
    }

    #[test]
    fn http_body_is_keyed_body_regardless_of_placeholder_name() {
        // The generated client always encodes the request body under the reserved
        // `body` key, not the placeholder's (schema type) name.
        let markers = vec![Marker::http_body(&BodyTemplate::Json(
            "SendMessageRequest".to_owned(),
        ))];
        let vals = decode_vals(markers, object(json!({"body": {"message": "hi"}}))).unwrap();

        assert_eq!(
            get_val(&vals, HTTP_BODY_FIELD)
                .unwrap()
                .into_json()
                .unwrap(),
            json!({"message": "hi"})
        );
    }

    #[test]
    fn empty_payload_with_no_markers_is_ok() {
        let vals = decode_vals(vec![], parse_payload_object(&[]).unwrap()).unwrap();
        assert!(vals.is_empty());
    }

    #[test]
    fn a_non_object_payload_is_rejected() {
        let err = parse_payload_object(b"[1,2,3]").unwrap_err();
        assert!(err.to_string().contains("object"), "got: {err}");
    }
}
