use cell_protocol::Sri;
use serde::{Deserialize, Serialize};

pub(crate) mod bridges;
pub(crate) mod restart;
pub(crate) mod tags;

pub use bridges::{
    BodyTemplate, HttpBridgeApi, HttpBridgeConfig, HttpBridgeRecord, MqttBridge, MqttBridgeConfig,
    MqttBridgeDef, MqttBridgeRecord, ResponseHeaderTemplate, TemplateSegment, TemplateSegments,
    WireHttpEndpoint, WireHttpRequestTemplate, WireHttpResponseTemplate, WireHttpResponseVariant,
    WireMqttEgress, WireMqttIngress, status_variant_name,
};
pub use restart::{RestartPolicy, RestartType, should_restart};
pub use tags::{RequirementTag, RequirementTags};

/// A batch of cells to deploy atomically. One entry for a single-cell or
/// spawned deploy; many for an app bundle. There is no separate "app" concept —
/// an app is just a set of cells that share an [`app`](CellDeployment::app) name.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct DeployRequest {
    pub cells: Vec<CellDeployment>,
}

impl DeployRequest {
    pub fn new(cells: Vec<CellDeployment>) -> Self {
        Self { cells }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellDeployment {
    pub sri: Sri,
    pub config: CellConfig,
    /// Capability tags the target runtime must satisfy (AND semantics). Empty
    /// means the cell can be placed on any runtime.
    pub(crate) tags: RequirementTags,
    /// Optional payload delivered to the cell's `#[init]` as its argument
    /// buffer. Set when a cell spawns another with `spawn_with`; `None` for
    /// cells deployed directly (from the CLI or as an app root).
    pub arguments: Option<Vec<u8>>,
    /// The app this cell belongs to. A root sets it explicitly — its own SRN for
    /// a standalone deploy, the bundle name for an app deploy. Spawned cells
    /// leave it `None` and inherit their parent's app at deploy time, so a whole
    /// spawn tree shares one app name.
    pub app: Option<String>,
    /// Who spawned this cell (identity + generation), detachment, and the
    /// spawn-time local name. Defaults for external/root deploys.
    pub lineage: crate::SpawnLineage,
    /// Restart policy for this cell. Only honored for roots (`lineage.parent`
    /// is `None`); non-roots recover via their parent's `on_cell_lost`. The
    /// default (`RestartType::Never`) preserves today's behavior.
    pub restart: RestartPolicy,
}

impl CellDeployment {
    /// Creates a cell deployment with no tag requirements (placeable anywhere).
    pub fn new(sri: Sri, config: CellConfig) -> Self {
        Self {
            sri,
            config,
            tags: RequirementTags::default(),
            arguments: None,
            app: None,
            lineage: crate::SpawnLineage::default(),
            restart: RestartPolicy::default(),
        }
    }

    pub fn tags(&self) -> &RequirementTags {
        &self.tags
    }

    /// Sets the capability tags the target runtime must satisfy.
    #[must_use]
    pub fn with_tags(mut self, tags: RequirementTags) -> Self {
        self.tags = tags;
        self
    }

    /// Sets the payload delivered to the cell's `#[init]` argument buffer.
    #[must_use]
    pub fn with_arguments(mut self, arguments: Option<Vec<u8>>) -> Self {
        self.arguments = arguments;
        self
    }

    /// Sets the app this cell belongs to. Leave `None` on a spawned cell to
    /// inherit the parent's app at deploy time.
    #[must_use]
    pub fn with_app(mut self, app: Option<String>) -> Self {
        self.app = app;
        self
    }

    /// Sets the spawn lineage (parent identity + generation, detachment,
    /// spawn-time local name).
    #[must_use]
    pub fn with_lineage(mut self, lineage: crate::SpawnLineage) -> Self {
        self.lineage = lineage;
        self
    }

    /// Sets the restart policy (honored only for roots).
    #[must_use]
    pub fn with_restart(mut self, restart: RestartPolicy) -> Self {
        self.restart = restart;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CellConfig {
    Wasm { class: String },
    HttpBridge(HttpBridgeApi),
    MqttBridge(MqttBridge),
}

impl std::fmt::Display for CellConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let details = match self {
            CellConfig::Wasm { class } => format!("Wasm({})", class),
            CellConfig::HttpBridge(bridge) => format!("Http({})", bridge.base_url),
            CellConfig::MqttBridge(bridge) => format!("Mqtt({})", bridge.broker),
        };
        f.write_str(&details)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cell_protocol::Sri;

    #[test]
    fn new_deployment_defaults_to_never_restart() {
        let d = CellDeployment::new(
            Sri::of_path("root").unwrap(),
            CellConfig::Wasm {
                class: "c".to_owned(),
            },
        );
        assert_eq!(d.restart.restart_type, RestartType::Never);
    }

    #[test]
    fn restart_policy_survives_postcard_round_trip() {
        let policy = RestartPolicy {
            restart_type: RestartType::OnError,
            max_restarts: 3,
            window_ms: 30_000,
            delay_ms: 2_000,
        };
        let d = CellDeployment::new(
            Sri::of_path("root").unwrap(),
            CellConfig::Wasm {
                class: "c".to_owned(),
            },
        )
        .with_restart(policy.clone());

        let bytes = postcard::to_allocvec(&d).unwrap();
        let back: CellDeployment = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back.restart, policy);
    }
}
