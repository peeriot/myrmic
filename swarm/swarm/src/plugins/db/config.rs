use serde::{Deserialize, Deserializer, Serialize};
use std::{path::PathBuf, time::Duration};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(flatten)]
    pub store: StoreConfig,

    #[serde(default)]
    pub load_from: Vec<LoadSpec>,

    /// Tags this node starts with, for the one set the whole node carries: it
    /// holds a replica of any configured set naming one of them, and may run
    /// any cell requiring them.
    ///
    /// Merged with the exec plugin's capability tags rather than overriding
    /// them — a node that runs an app's cells is usually the one that should
    /// hold its data. `myrmic tags` adds to and removes from the result
    /// without a restart, and a restart drops those changes for this list.
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoadSpec {
    #[serde(default)]
    pub scope: Option<String>,

    #[serde(default)]
    pub prefix: Option<String>,

    #[serde(default)]
    pub max_depth: Option<u32>,

    pub path: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StoreConfig {
    /// If omitted, the data is stored completely in-memory, and lost after restart.
    #[serde(default)]
    pub directory: Option<PathBuf>,

    /// GC scan interval as a humantime duration string (e.g. "100ms", "30s", "1min").
    /// Defaults to 60 seconds when omitted.
    #[serde(default, deserialize_with = "deserialize_optional_duration")]
    pub gc_interval: Option<Duration>,

    /// How long an RPC transaction may sit unused before the store rolls it back.
    /// Humantime duration string. Defaults to 5 minutes when omitted.
    #[serde(default, deserialize_with = "deserialize_optional_duration")]
    pub tx_idle_timeout: Option<Duration>,

    /// How long an offloader serves a scope no replica has taken over before it
    /// escalates itself into a durable replica. Humantime duration string.
    /// Defaults to 30 seconds when omitted.
    #[serde(default, deserialize_with = "deserialize_optional_duration")]
    pub offload_escalation_timeout: Option<Duration>,
}

fn deserialize_optional_duration<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
where
    D: Deserializer<'de>,
{
    let s: Option<String> = Option::deserialize(deserializer)?;
    match s {
        None => Ok(None),
        Some(s) => humantime::parse_duration(&s)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}
