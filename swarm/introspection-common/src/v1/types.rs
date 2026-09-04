//! Module defining the types which are known to both client and plugin

use serde_json::Value;
use serde_with::{json::JsonString, serde_as};
use zenoh::{Session, config::ZenohId};

/// Represents the information about the node provided by the introspection plugin hosted there
#[derive(serde::Deserialize, serde::Serialize, Eq, PartialEq, Hash, Clone, Debug)]
pub struct NodeStatus {
    pub id: ZenohId,
    /// Self-described identity of the node, when it gave one — always present
    /// for plugin-less participants (a CLI invocation), configured for
    /// plugin-hosting nodes the exec registry doesn't cover (a gateway).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub participant: Option<ParticipantInfo>,
    pub peers: Vec<ZenohId>,
    pub routers: Vec<ZenohId>,
    pub plugins: Vec<PluginInformation>,
}

impl NodeStatus {
    /// This session's status: its id and the links it currently holds, carrying
    /// whatever self-description and plugin list the caller has. The one place
    /// a status is assembled, so the plugin and a bare participant cannot drift
    /// apart as fields are added here.
    pub async fn of_session(
        session: &Session,
        participant: Option<ParticipantInfo>,
        plugins: Vec<PluginInformation>,
    ) -> Self {
        let info = session.info();
        Self {
            id: info.zid().await,
            participant,
            peers: info.peers_zid().await.collect(),
            routers: info.routers_zid().await.collect(),
            plugins,
        }
    }
}

/// Self-described identity of a network participant the exec registry doesn't
/// cover — a CLI invocation or a gateway, for example — so listings can label
/// it instead of showing a bare id.
#[derive(serde::Deserialize, serde::Serialize, Eq, PartialEq, Hash, Clone, Debug)]
pub struct ParticipantInfo {
    /// What sort of participant this is, e.g. `"cli"`.
    pub kind: String,
    /// What is running, e.g. the command line: `"m db monitor"`.
    pub name: String,
    /// Where it runs, as `user@host`, when discoverable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

/// Represents the information about a plugin hosted on the node in question
#[serde_as]
#[derive(serde::Deserialize, serde::Serialize, Eq, PartialEq, Hash, Clone, Debug)]
pub struct PluginInformation {
    pub name: String,
    #[serde_as(as = "JsonString")]
    pub config: Value,
}
