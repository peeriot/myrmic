use serde::{Deserialize, Serialize};
use std::{path::PathBuf, time::Duration};

use crate::plugins::deserialize_optional_duration;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

    /// Test hook: hold back everything replication brings into this node by
    /// this long before handling it, as a humantime duration string (e.g.
    /// "5s"): each announce and changeset it receives, and the first page of
    /// each of its own catch-up pulls. So its copies lag behind the other
    /// nodes' by about this long, while everything else between the nodes
    /// travels without delay: requests, transactions, and what this node
    /// serves the others' replication. The pacing of a pull of many pages
    /// counts the wait too, so the second page may come up to this long late
    /// once more.
    ///
    /// The other nodes broadcast their replication messages to every holder,
    /// so the receiving node is the only place where one node's copy alone can
    /// be held back. Only a build with the `test-hooks` feature knows the key.
    #[cfg(feature = "test-hooks")]
    #[serde(default, deserialize_with = "deserialize_optional_duration")]
    pub replication_delay: Option<Duration>,
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
#[serde(deny_unknown_fields)]
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

    /// How long an offloader serves a scope no replica fetches from before it
    /// escalates itself into a durable replica. Humantime duration string.
    /// Defaults to 30 seconds when omitted.
    #[serde(default, deserialize_with = "deserialize_optional_duration")]
    pub offload_escalation_timeout: Option<Duration>,
}
