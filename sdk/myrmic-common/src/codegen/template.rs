//! Bridge template types shared between codegen and the runtime.
//!
//! These types parse the `${kind:name}` (or `${db:ns/db/schema@key}`) placeholder
//! syntax used in bridge specs. They deserialize *from* the raw YAML string via
//! [`TryFrom<String>`] so on-disk format is just a string, but in memory the
//! structured variants are what get serialized when the manifest is later
//! persisted as a record.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub trait Seg: std::str::FromStr {
    fn split(&self) -> Option<(&'static str, &str)>;
}

pub trait RawSeg: Seg {
    fn raw(part: &str) -> Self;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Segments<T>(Vec<T>);

impl<T> Segments<T> {
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.0.iter()
    }

    pub fn into_segments(self) -> Vec<T> {
        self.0
    }
}

pub type TemplateSegments = Segments<TemplateSegment>;

impl<T: RawSeg<Err: Into<String>>> core::str::FromStr for Segments<T> {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut segments = Vec::new();
        let mut rest = s;

        while let Some(start) = rest.find("${") {
            if start > 0 {
                segments.push(T::raw(&rest[..start]));
            }
            let after = &rest[start..];
            let end = after
                .find('}')
                .ok_or_else(|| "unterminated `${` in template".to_string())?;
            let val = T::from_str(&after[..=end]).map_err(Into::into)?;
            segments.push(val);
            rest = &after[end + 1..];
        }

        if !rest.is_empty() {
            segments.push(T::raw(rest));
        }

        Ok(Self(segments))
    }
}

pub fn string_or<'de, T, D>(de: D) -> Result<T, D::Error>
where
    T: Deserialize<'de> + core::str::FromStr<Err = String>,
    D: Deserializer<'de>,
{
    use serde::de::Error;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either<T> {
        Str(String),
        Val(T),
    }
    match Either::<T>::deserialize(de)? {
        Either::Str(s) => s.parse().map_err(D::Error::custom),
        Either::Val(v) => Ok(v),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DbSegment {
    namespace: String,
    database: String,
    schema: String,
    key: String,
}

impl DbSegment {
    pub fn into_tuple(self) -> (String, String, String, String) {
        (self.namespace, self.database, self.schema, self.key)
    }

    fn parse(value: &str) -> Result<Self, String> {
        let (path, key) = value
            .split_once('@')
            .ok_or_else(|| format!("db placeholder missing `@key`: `{value}`"))?;
        let parts: Vec<&str> = path.split('/').collect();
        let [ns, db, schema] = parts.as_slice() else {
            return Err(format!("db placeholder expects ns/db/schema, got `{path}`"));
        };
        Ok(DbSegment {
            namespace: (*ns).to_string(),
            database: (*db).to_string(),
            schema: (*schema).to_string(),
            key: key.to_string(),
        })
    }
}

fn strip_single(s: &str) -> Result<&str, &'static str> {
    let s = s
        .trim()
        .strip_prefix("${")
        .ok_or("unable to locate `${`")?
        .strip_suffix('}')
        .ok_or("unable to locate `}`")?;

    if s.contains("${") || s.contains('}') {
        return Err("template must consist of a single segment");
    }

    Ok(s)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ResponseHeaderTemplate {
    String(String),
}

impl Seg for ResponseHeaderTemplate {
    fn split(&self) -> Option<(&'static str, &str)> {
        match self {
            Self::String(n) => Some(("string", n)),
        }
    }
}

impl core::str::FromStr for ResponseHeaderTemplate {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = strip_single(s)?;
        let (kind, value) = s
            .split_once(':')
            .ok_or_else(|| format!("missing `:` in `{s}`"))?;

        Ok(match kind {
            "string" => Self::String(value.to_string()),
            other => return Err(format!("unknown placeholder kind `{other}`")),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BodyTemplate {
    String(String),
    Json(String),
    Bytes(String),
}

impl Seg for BodyTemplate {
    fn split(&self) -> Option<(&'static str, &str)> {
        match self {
            Self::String(n) => Some(("string", n)),
            Self::Json(n) => Some(("json", n)),
            Self::Bytes(n) => Some(("bytes", n)),
        }
    }
}

impl core::str::FromStr for BodyTemplate {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = strip_single(s)?;
        let (kind, value) = s
            .split_once(':')
            .ok_or_else(|| format!("missing `:` in `{s}`"))?;

        Ok(match kind {
            "string" => Self::String(value.to_string()),
            "json" => Self::Json(value.to_string()),
            "bytes" => Self::Bytes(value.to_string()),
            other => return Err(format!("unknown placeholder kind `{other}`")),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TemplateSegment {
    Raw(String),
    Db(DbSegment),

    String(String),
    Json(String),
    Bool(String),
    U8(String),
    U16(String),
    U32(String),
    U64(String),
    I8(String),
    I16(String),
    I32(String),
    I64(String),
    F32(String),
    F64(String),
}

impl TemplateSegment {
    pub fn is_encoded(&self) -> bool {
        !matches!(self, Self::Raw(_) | Self::Db(_))
    }
}

impl Seg for TemplateSegment {
    fn split(&self) -> Option<(&'static str, &str)> {
        match self {
            Self::String(n) => Some(("string", n)),
            Self::Json(n) => Some(("json", n)),
            Self::Bool(n) => Some(("bool", n)),
            Self::U8(n) => Some(("u8", n)),
            Self::U16(n) => Some(("u16", n)),
            Self::U32(n) => Some(("u32", n)),
            Self::U64(n) => Some(("u64", n)),
            Self::I8(n) => Some(("i8", n)),
            Self::I16(n) => Some(("i16", n)),
            Self::I32(n) => Some(("i32", n)),
            Self::I64(n) => Some(("i64", n)),
            Self::F32(n) => Some(("f32", n)),
            Self::F64(n) => Some(("f64", n)),
            Self::Raw(_) | Self::Db(_) => None,
        }
    }
}

impl RawSeg for TemplateSegment {
    fn raw(part: &str) -> Self {
        Self::Raw(part.to_string())
    }
}

impl core::str::FromStr for TemplateSegment {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = strip_single(s)?;
        let (kind, value) = s
            .split_once(':')
            .ok_or_else(|| format!("missing `:` in `{s}`"))?;

        Ok(match kind {
            "string" => Self::String(value.to_string()),
            "json" => Self::Json(value.to_string()),
            "bool" => Self::Bool(value.to_string()),
            "u8" => Self::U8(value.to_string()),
            "u16" => Self::U16(value.to_string()),
            "u32" => Self::U32(value.to_string()),
            "u64" => Self::U64(value.to_string()),
            "i8" => Self::I8(value.to_string()),
            "i16" => Self::I16(value.to_string()),
            "i32" => Self::I32(value.to_string()),
            "i64" => Self::I64(value.to_string()),
            "f32" => Self::F32(value.to_string()),
            "f64" => Self::F64(value.to_string()),
            "db" => Self::Db(DbSegment::parse(value)?),
            other => return Err(format!("unknown placeholder kind `{other}`")),
        })
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct ParseInto<T>(pub T);

impl<T: Serialize> Serialize for ParseInto<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de, T> Deserialize<'de> for ParseInto<T>
where
    T: Deserialize<'de> + std::str::FromStr<Err: std::fmt::Display>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Either<T> {
            Str(String),
            Val(T),
        }
        let value = match Either::<T>::deserialize(deserializer)? {
            Either::Str(s) => s.parse().map_err(<D::Error as serde::de::Error>::custom)?,
            Either::Val(v) => v,
        };
        Ok(Self(value))
    }
}

impl<T: core::str::FromStr> core::str::FromStr for ParseInto<T> {
    type Err = T::Err;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(T::from_str(s)?))
    }
}
