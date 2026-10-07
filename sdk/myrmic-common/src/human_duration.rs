//! Durations written the way people say them: `500ms`, `1s`, `2min`.
//!
//! Use with `#[serde(with = "myrmic_common::human_duration")]`, or
//! `human_duration::option` for an optional one. A duration stays text in
//! every format, so it reads the same in YAML and survives postcard, which is
//! not self-describing, unchanged.

use core::time::Duration;
use serde::{Deserialize, Deserializer, Serializer};

/// Writes `duration` as text, e.g. `1s 500ms`.
pub fn serialize<S: Serializer>(duration: &Duration, ser: S) -> Result<S::Ok, S::Error> {
    ser.collect_str(&humantime::format_duration(*duration))
}

/// Reads a duration from text like `500ms` or `2min`.
pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Duration, D::Error> {
    let text = String::deserialize(de)?;
    humantime::parse_duration(&text)
        .map_err(|err| serde::de::Error::custom(format!("invalid duration `{text}`: {err}")))
}

/// The same for an optional duration.
pub mod option {
    use core::time::Duration;
    use serde::{Deserialize, Deserializer, Serializer};

    /// Writes `duration` as text, e.g. `1s 500ms`, or nothing.
    pub fn serialize<S: Serializer>(
        duration: &Option<Duration>,
        ser: S,
    ) -> Result<S::Ok, S::Error> {
        match duration {
            Some(duration) => {
                ser.serialize_some(&humantime::format_duration(*duration).to_string())
            }
            None => ser.serialize_none(),
        }
    }

    /// Reads an optional duration from text like `500ms` or `2min`.
    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Option<Duration>, D::Error> {
        Option::<String>::deserialize(de)?
            .map(|text| {
                humantime::parse_duration(&text).map_err(|err| {
                    serde::de::Error::custom(format!("invalid duration `{text}`: {err}"))
                })
            })
            .transpose()
    }
}
