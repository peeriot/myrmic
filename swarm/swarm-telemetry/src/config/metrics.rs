use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub otel_endpoint: Option<String>,
    /// Overrides the periodic metric reader's export interval, otherwise the `OTel` SDK default
    /// (60s) — long enough that a short-lived load benchmark's drain window can end before a
    /// single periodic export ever fires, leaving every metric-derived report field empty even
    /// when nothing is actually broken.
    #[serde(default)]
    pub export_interval_ms: Option<u64>,
}
