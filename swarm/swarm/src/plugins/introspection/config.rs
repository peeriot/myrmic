use introspection_common::v1::ParticipantInfo;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    // interval of node metrics update in seconds
    #[serde(default = "default_metric_update_interval")]
    pub(crate) metric_update_interval: u64,

    /// Self-description carried in this node's status, for nodes the exec
    /// registry doesn't cover — a gateway, say. Set programmatically only;
    /// `deny_unknown_fields` above turns a `participant` key in a config file
    /// into a startup error rather than a value that silently goes nowhere.
    #[serde(skip)]
    pub participant: Option<ParticipantInfo>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            metric_update_interval: default_metric_update_interval(),
            participant: None,
        }
    }
}

/// the default interval for node metric collection is 10s
fn default_metric_update_interval() -> u64 {
    10
}
