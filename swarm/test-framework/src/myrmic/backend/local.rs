use std::sync::Arc;

use super::{MyrmicBackend, isolated_multicast_group, isolated_runtime_config};

/// [`MyrmicBackend`] that runs programs on the host, isolated or connected to an endpoint or
/// neither (see [`crate::myrmic::Myrmic::local_isolated`] and
/// [`crate::myrmic::Myrmic::local_connected`]).
#[derive(Clone, Default)]
pub struct Local {
    discovery: Discovery,
}

/// How the programs find the swarm.
#[derive(Clone, Default)]
enum Discovery {
    /// on the host's state directories and the default multicast group
    #[default]
    Host,
    /// in an isolation; shared by the clones, so its directories live as long as any runtime or
    /// cell handle that may still need them for cleanup
    Isolated(Arc<Isolation>),
    /// through this endpoint (e.g. `tcp/192.0.2.10:7447`), with no multicast scouting; such a
    /// shim joins a swarm that runs elsewhere and starts no runtime itself
    Connected(String),
}

/// A private myrmic setup on the host (see [`crate::myrmic::Myrmic::local_isolated`]).
struct Isolation {
    /// holds `data/` (`XDG_DATA_HOME`: runtime identities, databases, logs) and `run/`
    /// (`XDG_RUNTIME_DIR`: the pid files `runtimes list` and `delete` read); shared with the
    /// isolations [`Local::cut_off`] creates
    dir: Arc<tempfile::TempDir>,
    /// the multicast group (`<address>:<port>`) its runtimes scout on, and the CLI and
    /// sessions from [`Local::zenoh_config`] discover them by
    multicast_group: String,
}

impl Local {
    /// programs run with their own state directories and multicast group
    pub(crate) fn isolated() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("myrmic-isolated.")
            .tempdir()
            .expect("failed to create a temporary directory for an isolated myrmic");
        for subdir in ["data", "run"] {
            std::fs::create_dir(dir.path().join(subdir))
                .unwrap_or_else(|err| panic!("failed to create `{subdir}` in {dir:?}: {err}"));
        }
        Self {
            discovery: Discovery::Isolated(Arc::new(Isolation {
                dir: Arc::new(dir),
                multicast_group: isolated_multicast_group(),
            })),
        }
    }

    /// programs reach the swarm through `endpoint` instead of by multicast scouting
    pub(crate) fn connected(endpoint: String) -> Self {
        Self {
            discovery: Discovery::Connected(endpoint),
        }
    }

    /// this isolated backend's state directories, on a multicast group of its own
    pub(crate) fn cut_off(&self) -> Self {
        let Discovery::Isolated(isolation) = &self.discovery else {
            panic!("only an isolated shim has state directories to share");
        };
        Self {
            discovery: Discovery::Isolated(Arc::new(Isolation {
                dir: isolation.dir.clone(),
                multicast_group: isolated_multicast_group(),
            })),
        }
    }

    /// A zenoh peer config that finds the swarm the way the CLI does, e.g. for a session to
    /// create [`crate::clients::db::DbHandle`] or [`crate::clients::sorg::SorgHandle`] with.
    ///
    /// It only reaches runtimes this host reaches by scouting or the endpoint. A remote mesh
    /// needs a route in instead, e.g. rack's SSH tunnel to a pinned listen port.
    pub fn zenoh_config(&self) -> zenoh::Config {
        let mut config = zenoh::Config::default();
        config
            .set_mode(Some(zenoh::config::WhatAmI::Peer))
            .expect("setting zenoh mode cannot fail");
        match &self.discovery {
            Discovery::Host => {}
            Discovery::Isolated(isolation) => {
                config
                    .insert_json5(
                        "scouting/multicast/address",
                        &serde_json::json!(isolation.multicast_group).to_string(),
                    )
                    .expect("the isolated multicast group is a valid zenoh multicast address");
            }
            Discovery::Connected(endpoint) => {
                config
                    .insert_json5(
                        "connect/endpoints",
                        &serde_json::json!([endpoint]).to_string(),
                    )
                    .unwrap_or_else(|err| panic!("invalid endpoint `{endpoint}`: {err}"));
                config
                    .insert_json5("scouting/multicast/enabled", "false")
                    .expect("disabling multicast scouting cannot fail");
            }
        }
        config
    }
}

impl MyrmicBackend for Local {
    /// `program args`, finding the swarm the way this backend does:
    /// - in an isolation, with its state directories as `XDG_DATA_HOME` and `XDG_RUNTIME_DIR`,
    ///   and its multicast group as `DEFAULT_MYRMIC_MULTICAST_GROUP`, which the CLI takes like
    ///   `--multicast-group`;
    /// - connected, with the endpoint as `DEFAULT_MYRMIC_CONNECT`, which the CLI takes like
    ///   `--connect`.
    ///
    /// The variables are inherited, so they reach the CLI also when it is exec'd by `sh`.
    fn command(&self, program: &str, args: &[&str]) -> std::process::Command {
        let mut command = std::process::Command::new(program);
        command.args(args);
        match &self.discovery {
            Discovery::Host => {}
            Discovery::Isolated(isolation) => {
                command
                    .env("XDG_DATA_HOME", isolation.dir.path().join("data"))
                    .env("XDG_RUNTIME_DIR", isolation.dir.path().join("run"))
                    .env("DEFAULT_MYRMIC_MULTICAST_GROUP", &isolation.multicast_group);
            }
            Discovery::Connected(endpoint) => {
                command.env("DEFAULT_MYRMIC_CONNECT", endpoint);
            }
        }
        command
    }

    fn wrapper_exit_codes(&self) -> &'static [i32] {
        // there is no wrapper: a program that cannot be started is a spawn error
        &[]
    }

    /// in an isolation, the config that lets the runtime scout on the isolation's multicast group
    fn runtime_config(&self) -> Option<String> {
        match &self.discovery {
            Discovery::Host => None,
            Discovery::Isolated(isolation) => {
                Some(isolated_runtime_config(&isolation.multicast_group))
            }
            Discovery::Connected(_) => {
                panic!("a shim connected to a swarm elsewhere starts no runtime")
            }
        }
    }
}
