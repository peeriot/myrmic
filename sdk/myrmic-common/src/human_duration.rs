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

/// A [`Duration`] that is not zero, as [`NonZeroU64`](core::num::NonZeroU64) is a
/// `u64` that is not zero. Written like any other duration here; `0s` does not
/// parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NonZeroDuration(Duration);

impl NonZeroDuration {
    /// `duration`, unless it is zero.
    pub const fn new(duration: Duration) -> Option<Self> {
        if duration.is_zero() {
            None
        } else {
            Some(Self(duration))
        }
    }

    /// The duration, which is not zero.
    pub const fn get(self) -> Duration {
        self.0
    }
}

impl From<NonZeroDuration> for Duration {
    fn from(duration: NonZeroDuration) -> Self {
        duration.0
    }
}

impl serde::Serialize for NonZeroDuration {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        serialize(&self.0, ser)
    }
}

impl<'de> Deserialize<'de> for NonZeroDuration {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        Self::new(deserialize(de)?)
            .ok_or_else(|| serde::de::Error::custom("duration must be greater than 0"))
    }
}
