use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub otel_endpoint: Option<String>,
    #[serde(default)]
    pub batch: crate::config::batch::BatchConfig,
}
