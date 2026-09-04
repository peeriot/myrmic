use serde::{Deserialize, Serialize};

use crate::db::Scope;
use alloc::string::String;
use alloc::vec::Vec;

/// A time-series timestamp in NTP64 form, as produced by the swarm's hybrid
/// logical clock: the upper 32 bits are whole seconds since `UNIX_EPOCH`, the
/// lower 32 bits the fractional second.
pub type Timestamp = u64;

/// Wire request to write a measurement into the time-series store.
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct PublishRequest {
    pub scope: Scope,
    pub measurement: Measurement,
}

/// One time-series data point: a named set of tagged field values.
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct Measurement {
    pub name: String,
    /// Indexed `(key, value)` metadata identifying the series.
    pub tags: Vec<(String, String)>,
    /// The measured `(name, value)` data itself.
    pub fields: Vec<(String, FieldValue)>,
    /// When the measurement was taken; `None` lets the host stamp it with
    /// its hybrid logical clock on arrival.
    pub ts: Option<Timestamp>,
}

/// The value of one measurement field.
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub enum FieldValue {
    I64(i64),
    U64(u64),
    F64(f64),
    String(String),
    Boolean(bool),
}

/// Direction time-series samples are returned in, ordered by timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TsOrderBy {
    /// Oldest first.
    TimestampAsc,
    /// Newest first (the default).
    #[default]
    TimestampDesc,
}

/// Wire request to query samples of a measurement.
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct FindRequest {
    pub scope: Scope,
    pub measurement_name: String,
    /// Maximum number of samples to return; `None` for no limit.
    pub limit: Option<usize>,
    /// Inclusive
    pub start: Option<Timestamp>,
    /// Exclusive
    pub end: Option<Timestamp>,
    /// Order of returned samples. `None` uses the default (newest-first).
    pub order: Option<TsOrderBy>,
}

/// Wire response to a [`FindRequest`].
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct FindResponse {
    pub samples: Vec<Sample>,
}

/// One stored time-series sample, as returned by a query.
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct Sample {
    /// The series-identifying `(key, value)` tags.
    pub tags: Vec<(String, String)>,
    /// The measured `(name, value)` data.
    pub fields: Vec<(String, FieldValue)>,
    pub timestamp: Timestamp,
}
