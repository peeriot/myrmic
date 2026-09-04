use serde::{Deserialize, Serialize};

#[derive(Default, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SwarmConfig {
    #[serde(default)]
    pub zenoh: zenoh::Config,

    #[serde(default)]
    pub telemetry: swarm_telemetry::TelemetryConfig,

    #[serde(flatten)]
    pub plugins: PluginConfigs,
}

#[derive(Default, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginConfigs {
    #[cfg(feature = "plugin-db")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db: Option<<crate::plugins::db::Plugin as crate::plugins::MyrmicPlugin>::Config>,
    #[cfg(feature = "plugin-mqtt")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mqtt: Option<<crate::plugins::mqtt::Plugin as crate::plugins::MyrmicPlugin>::Config>,
    #[cfg(feature = "plugin-orchestration")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestration: Option<
        <crate::plugins::orchestration::SorgOrchestrationPlugin as crate::plugins::MyrmicPlugin>::Config,
    >,
    #[cfg(feature = "plugin-execution")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution:
        Option<<crate::plugins::execution::SorgExecutionPlugin as crate::plugins::MyrmicPlugin>::Config>,
    #[cfg(feature = "plugin-gateway")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<<crate::plugins::gateway::Plugin as crate::plugins::MyrmicPlugin>::Config>,
    #[cfg(feature = "plugin-introspection")]
    #[serde(default)]
    pub introspection:
        <crate::plugins::introspection::IntrospectionPlugin as crate::plugins::MyrmicPlugin>::Config,
    #[cfg(feature = "plugin-onboarding")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub onboarding: Option<
        <crate::plugins::onboarding::SwarmOnboardingPlugin as crate::plugins::MyrmicPlugin>::Config,
    >,
    #[cfg(feature = "plugin-test-control")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_control: Option<
        <crate::plugins::test_control::ZenohTestControlPlugin as crate::plugins::MyrmicPlugin>::Config,
    >,
    #[cfg(feature = "plugin-embedded-log")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedded_log: Option<
        <crate::plugins::embedded_log::EmbeddedLoggingPlugin as crate::plugins::MyrmicPlugin>::Config,
    >,
}

#[cfg(test)]
mod tests {
    use super::SwarmConfig;

    /// The rack harness writes a `zenoh` section into every host's config
    /// (`test-framework`'s `upload_host_config`). `SwarmConfig` denies unknown
    /// fields, so a key that has drifted from zenoh's schema stops every
    /// runtime from starting — a whole rack run lost to a typo, with the
    /// failure surfacing only as hosts that never register. Pin the shape.
    ///
    /// A `zenoh.transport.link.tx.queue.batching.enabled: false` override was
    /// measured here (run 33407467208) and reverted: it moved a hop by +-2ms at
    /// loads 100-500 and made load 750 2.6x worse, batching being what absorbs
    /// back-pressure. The path is correct if it is ever wanted again.
    #[test]
    fn the_racks_zenoh_section_deserializes() {
        let yaml = "zenoh:\n\
                    \x20\x20listen:\n\
                    \x20\x20\x20\x20endpoints:\n\
                    \x20\x20\x20\x20\x20\x20peer:\n\
                    \x20\x20\x20\x20\x20\x20\x20\x20- \"tcp/[::]:7447\"\n";

        let config: SwarmConfig =
            serde_yaml::from_str(yaml).expect("the rack's host config must deserialize");

        // Round-trip rather than reach through zenoh's mode-dependent
        // accessors: what matters is that the endpoint survived rather than
        // being quietly dropped into a default.
        let round_tripped =
            serde_yaml::to_string(&config.zenoh).expect("zenoh config must re-serialize");
        assert!(
            round_tripped.contains("tcp/[::]:7447"),
            "the listen endpoint must survive deserialization, got:\n{round_tripped}",
        );
    }
}
