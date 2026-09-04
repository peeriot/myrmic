//! Spawning swarm instances (local process or docker image) for e2e tests.

use std::path::{Path, PathBuf};

use bollard::Docker;

use crate::docker::image::Image;

pub mod backend;
pub mod process;

pub use backend::{LocalBinary, SwarmBackend};
pub use process::SwarmProcess;

/// Builds a swarm docker image. Used in compose-based e2e tests.
pub struct SwarmImage {}

impl SwarmImage {
    /// Build a docker image with tag `tag` from `swarm_dockerfile`, placing the swarm binary
    /// (as `swarm`) and any `extra_files` (`(host path, name in context)` pairs) in the build
    /// context.
    pub async fn build(
        docker: &Docker,
        swarm_dockerfile: impl Into<PathBuf>,
        swarm_binary: impl Into<PathBuf>,
        tag: &str,
        extra_files: &[(&Path, &str)],
    ) {
        let swarm_binary = swarm_binary.into();
        let mut files = vec![(swarm_binary.as_path(), "swarm")];
        files.extend_from_slice(extra_files);
        let _image = Image::build(
            docker,
            tag,
            swarm_dockerfile.into().iter().as_path(),
            &files,
        )
        .await;
    }
}

/// Opens a zenoh client session connected to `endpoint` (e.g. `tcp/127.0.0.1:1234`), retrying
/// until it accepts the connection or giving up after ~30s.
///
/// Shared by [`backend::local::LocalBinary`] (an ephemeral localhost port for a freshly spawned
/// child process) and [`crate::rack`] (a real network endpoint, typically reached through an SSH
/// tunnel) — both need the exact same "keep trying until the other side is actually listening"
/// behavior, just against a different address.
pub(crate) async fn open_client_session(endpoint: &str) -> zenoh::Session {
    tryhard::retry_fn(|| async {
        let mut config = zenoh::Config::default();
        config
            .insert_json5("mode", r#""client""#)
            .expect("zenoh mode");
        config
            .insert_json5("connect/endpoints", &format!(r#"["{endpoint}"]"#))
            .expect("zenoh endpoints");
        config
            .insert_json5("scouting/multicast/enabled", "false")
            .expect("zenoh multicast");
        config
            .insert_json5("open/return_conditions/connect_scouted", "true")
            .expect("zenoh connect_scouted");

        tokio::time::timeout(std::time::Duration::from_secs(2), zenoh::open(config))
            .await
            .map_err(|_| ())
            .and_then(|opened| opened.map_err(|_| ()))
    })
    .retries(150)
    .fixed_backoff(std::time::Duration::from_millis(200))
    .await
    .unwrap_or_else(|()| {
        panic!("timed out waiting for a zenoh peer to accept connections on {endpoint}")
    })
}

/// Entry point for spawning swarm instances on a [`SwarmBackend`].
pub struct Swarm<B> {
    backend: B,
}

impl Swarm<LocalBinary> {
    /// use a locally built swarm binary (see [`crate::resolve_binary!`])
    pub fn local() -> Self {
        Self {
            backend: LocalBinary::new(crate::resolve_binary!("swarm")),
        }
    }
}

impl<B: SwarmBackend> Swarm<B> {
    /// spawn a swarm instance with the jsonnet config at `config_path`
    pub async fn spawn(&self, config_path: impl AsRef<Path>) -> SwarmProcess {
        self.backend.spawn(config_path.as_ref()).await
    }
}
