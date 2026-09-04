use indexmap::IndexMap;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct PipelineFile {
    pub pipeline: PipelineInfo,
    #[serde(default)]
    pub sources: Vec<Source>,
    #[serde(default)]
    pub steps: Vec<Step>,
    #[serde(default)]
    pub taps: Vec<Tap>,
    #[serde(default)]
    pub outlets: Vec<Outlet>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PipelineInfo {
    pub id: String,
}

/// A sensor source that reads from a device declared in the board manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct Source {
    pub id: String,
    /// References a device `id` in the board manifest.
    pub device: String,
    /// Application-tier config only (e.g. `sample_interval_ms`).
    /// Hardware fields (e.g. `i2c_addr`) must not appear here.
    #[serde(default)]
    pub config: IndexMap<String, serde_yaml::Value>,
}

/// A named, typed write-side slot bound to an output device — the write-side
/// mirror of a [`Tap`]. A cell (or, later, an in-layer step) writes commands to
/// `name`; the driver backing `device` consumes them. Retained-only for v1.
#[derive(Debug, Clone, Deserialize)]
pub struct Outlet {
    pub name: String,
    #[serde(rename = "type")]
    pub type_name: String,
    /// References the output device `id` in the board manifest that consumes
    /// this outlet. Exactly one outlet may target a given device (single-writer).
    pub device: String,
    /// Optional in-layer feed-forward source: a `"<source>.<field>"` field or a
    /// step id whose output type equals this outlet's command type. When set,
    /// the outlet is **pipeline-driven** — a WASM cell cannot write it (it is not
    /// registered in the outlet registry), and codegen applies the value inline
    /// in the producing source task. When absent, the outlet is **cell-driven**
    /// (registered, served by a dedicated sink task).
    #[serde(default)]
    pub input: Option<String>,
    /// Application-tier config for the output device (e.g. write cadence).
    /// Hardware fields (e.g. pin wiring) live in the board manifest.
    #[serde(default)]
    pub config: IndexMap<String, serde_yaml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Step {
    pub id: String,
    pub op: String,
    pub input: String,
    #[serde(default)]
    pub config: IndexMap<String, serde_yaml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Tap {
    pub name: String,
    pub kind: TapKind,
    #[serde(rename = "type")]
    pub type_name: String,
    pub source: String,
    #[serde(default)]
    pub stream_kind: TapStreamKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TapKind {
    Retained,
    Event,
    Batch,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TapStreamKind {
    Signal,
    #[default]
    Metric,
}
