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

/// Fills the zenoh settings a runtime needs to route across a non-clique
/// topology, where a plain zenoh default would leave every node a peer.
///
/// Only values still at zenoh's default are touched, so an explicit setting
/// always wins. An unset mode becomes `router`; a router then listens on an
/// ephemeral port and multicast-autoconnects to every kind of node. Nothing
/// else is changed when the mode is `peer` or `client`.
///
/// # Errors
///
/// Fails if zenoh rejects reading or writing one of the settings.
pub fn apply_runtime_defaults(zenoh: &mut zenoh::Config) -> anyhow::Result<()> {
    if read_json(zenoh, "mode")?.is_null() {
        zenoh
            .set_mode(Some(zenoh::config::WhatAmI::Router))
            .map_err(|mode| anyhow::anyhow!("unable to set zenoh mode {mode:?}"))?;
    }
    if read_json(zenoh, "mode")? != serde_json::json!("router") {
        return Ok(());
    }

    let defaults = zenoh::Config::default();
    let routes = [
        ("listen/endpoints", r#"["tcp/[::]:0"]"#),
        (
            "scouting/multicast/autoconnect",
            r#"["router","peer","client"]"#,
        ),
    ];
    for (key, value) in routes {
        let mut current = read_json(zenoh, key)?;
        // A plain list applies to every mode and is the user's own choice.
        if current.is_array() {
            continue;
        }
        // A missing router entry (e.g. only `peer` is set) is unset, not explicit.
        let default = read_json(&defaults, key)?.get("router").cloned();
        let router = current.get("router").filter(|router| !router.is_null());
        if router.is_some() && router != default.as_ref() {
            continue;
        }
        if current.is_null() {
            current = serde_json::json!({});
        }
        current["router"] = serde_json::from_str(value)?;
        set_json(zenoh, key, &current.to_string())?;
    }

    Ok(())
}

fn read_json(zenoh: &zenoh::Config, key: &str) -> anyhow::Result<serde_json::Value> {
    let json = zenoh
        .get_json(key)
        .map_err(|err| anyhow::anyhow!("unable to read zenoh {key}: {err}"))?;

    serde_json::from_str(&json).map_err(|err| anyhow::anyhow!("unable to parse zenoh {key}: {err}"))
}

fn set_json(zenoh: &mut zenoh::Config, key: &str, value: &str) -> anyhow::Result<()> {
    zenoh
        .insert_json5(key, value)
        .map_err(|err| anyhow::anyhow!("unable to set zenoh {key}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::{SwarmConfig, apply_runtime_defaults};

    #[test]
    fn the_defaults_make_an_unset_mode_a_router() {
        let mut zenoh = zenoh::Config::default();
        apply_runtime_defaults(&mut zenoh).expect("defaults apply");

        assert_eq!(json(&zenoh, "mode"), "router");
        assert_eq!(
            json(&zenoh, "listen/endpoints")["router"],
            serde_json::json!(["tcp/[::]:0"])
        );
        assert_eq!(
            json(&zenoh, "scouting/multicast/autoconnect")["router"],
            serde_json::json!(["router", "peer", "client"])
        );
        assert_eq!(
            json(&zenoh, "scouting/gossip/autoconnect"),
            json(&zenoh::Config::default(), "scouting/gossip/autoconnect")
        );
    }

    #[test]
    fn an_explicit_peer_or_client_is_left_alone() {
        for mode in ["peer", "client"] {
            let mut zenoh = zenoh_from(&format!("zenoh:\n  mode: {mode}\n"));
            let before = serde_json::to_value(&zenoh).expect("serializes");
            apply_runtime_defaults(&mut zenoh).expect("defaults apply");

            assert_eq!(serde_json::to_value(&zenoh).expect("serializes"), before);
            assert_eq!(json(&zenoh, "mode"), mode);
        }
    }

    #[test]
    fn explicit_router_values_are_left_alone() {
        let mut zenoh = zenoh_from(
            "zenoh:\n  listen:\n    endpoints:\n      router:\n        - \"tcp/[::]:7448\"\n  \
             scouting:\n    multicast:\n      autoconnect:\n        router: [\"router\"]\n",
        );
        apply_runtime_defaults(&mut zenoh).expect("defaults apply");

        assert_eq!(
            json(&zenoh, "listen/endpoints")["router"],
            serde_json::json!(["tcp/[::]:7448"])
        );
        assert_eq!(
            json(&zenoh, "scouting/multicast/autoconnect")["router"],
            serde_json::json!(["router"])
        );
    }

    #[test]
    fn an_explicitly_empty_router_list_stays_empty() {
        let mut zenoh = zenoh_from("zenoh:\n  listen:\n    endpoints:\n      router: []\n");
        apply_runtime_defaults(&mut zenoh).expect("defaults apply");

        assert_eq!(
            json(&zenoh, "listen/endpoints")["router"],
            serde_json::json!([])
        );
    }

    #[test]
    fn a_peer_only_list_still_gets_a_router_listener() {
        let mut zenoh =
            zenoh_from("zenoh:\n  listen:\n    endpoints:\n      peer: [\"tcp/[::]:7447\"]\n");
        apply_runtime_defaults(&mut zenoh).expect("defaults apply");

        assert_eq!(
            json(&zenoh, "listen/endpoints")["router"],
            serde_json::json!(["tcp/[::]:0"])
        );
    }

    #[test]
    fn a_plain_list_is_left_alone() {
        let mut zenoh =
            zenoh_from("zenoh:\n  listen:\n    endpoints:\n      - \"tcp/[::]:7447\"\n");
        apply_runtime_defaults(&mut zenoh).expect("defaults apply");

        assert_eq!(json(&zenoh, "mode"), "router");
        assert_eq!(
            json(&zenoh, "listen/endpoints"),
            serde_json::json!(["tcp/[::]:7447"])
        );
    }

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
                    \x20\x20\x20\x20\x20\x20- \"tcp/[::]:7447\"\n";

        let mut config: SwarmConfig =
            serde_yaml::from_str(yaml).expect("the rack's host config must deserialize");
        apply_runtime_defaults(&mut config.zenoh).expect("defaults apply");

        // Round-trip rather than reach through zenoh's mode-dependent
        // accessors: what matters is that the endpoint survived rather than
        // being quietly dropped into a default.
        let round_tripped =
            serde_yaml::to_string(&config.zenoh).expect("zenoh config must re-serialize");
        assert!(
            round_tripped.contains("tcp/[::]:7447"),
            "the listen endpoint must survive deserialization, got:\n{round_tripped}",
        );
        // A plain list applies in every mode, so the defaults must not add a router list.
        assert_eq!(
            json(&config.zenoh, "listen/endpoints"),
            serde_json::json!(["tcp/[::]:7447"])
        );
    }

    #[cfg(feature = "plugin-onboarding")]
    #[test]
    fn the_onboarding_timeout_deserializes() {
        use std::time::Duration;

        let cases: [(&str, Option<Option<Duration>>); 4] = [
            ("{}", None),
            ("onboarding: {}", Some(None)),
            (
                "onboarding:\n  timeout: \"30s\"",
                Some(Some(Duration::from_secs(30))),
            ),
            ("onboarding:\n  timeout: null", Some(None)),
        ];
        for (yaml, expected) in cases {
            let config: SwarmConfig = serde_yaml::from_str(yaml)
                .unwrap_or_else(|err| panic!("{yaml:?} must deserialize: {err}"));
            let timeout = config
                .plugins
                .onboarding
                .map(|onboarding| onboarding.timeout);
            assert_eq!(timeout, expected, "{yaml:?}");
        }

        let err = serde_yaml::from_str::<SwarmConfig>("onboarding:\n  timeout: \"bananas\"")
            .expect_err("an unparsable timeout must be rejected");
        assert!(err.to_string().contains("expected number"), "{err}");
    }

    /// Nothing in this tree loads the configs in `swarm/configs`, so a plugin key
    /// that has been removed or a section written at the wrong nesting level
    /// survives review and turns into a runtime that exits during startup.
    #[test]
    fn every_shipped_config_parses() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../configs");
        let mut checked = 0;

        for entry in std::fs::read_dir(&dir).expect("the configs directory must be readable") {
            let path = entry.expect("the directory entry must be readable").path();
            if path.extension().is_none_or(|ext| ext != "jsonnet") {
                continue;
            }

            if let Err(err) = crate::eval_input::<SwarmConfig>(&path) {
                panic!("{} must parse as a SwarmConfig: {err}", path.display());
            }
            checked += 1;
        }

        assert!(checked > 0, "no configs found in {}", dir.display());
    }

    fn zenoh_from(yaml: &str) -> zenoh::Config {
        let config: SwarmConfig = serde_yaml::from_str(yaml).expect("config must deserialize");

        config.zenoh
    }

    fn json(zenoh: &zenoh::Config, key: &str) -> serde_json::Value {
        serde_json::from_str(&zenoh.get_json(key).expect("key must exist")).expect("valid json")
    }
}
