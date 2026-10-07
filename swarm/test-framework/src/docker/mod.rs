//! Thin docker wrappers (images, containers, network shaping) on top of [`bollard`].

use std::ops::Deref;

use bollard::Docker;

pub mod container;
pub mod image;
pub mod managed;

/// The docker daemon the framework talks to: a bollard client plus the endpoint it connects to,
/// so the `docker` CLI can be pointed at the same daemon (on its own the CLI follows the active
/// docker context, not the socket bollard picked).
#[derive(Clone)]
pub struct DockerDaemon {
    client: Docker,
    /// `DOCKER_HOST`-style URL, e.g. `unix:///var/run/docker.sock`
    host: String,
}

impl Deref for DockerDaemon {
    type Target = Docker;

    fn deref(&self) -> &Self::Target {
        &self.client
    }
}

impl DockerDaemon {
    /// the `docker` CLI, pointed at this daemon
    pub(crate) fn cli(&self) -> std::process::Command {
        let mut command = std::process::Command::new("docker");
        command.env("DOCKER_HOST", &self.host);
        command
    }
}

/// Connect to the docker daemon, honoring `DOCKER_HOST` and rootless
/// (`$XDG_RUNTIME_DIR/docker.sock`) setups before falling back to the system socket.
pub fn init_docker() -> DockerDaemon {
    let host = std::env::var("DOCKER_HOST")
        .ok()
        .or_else(rootless_socket)
        .unwrap_or_else(|| SYSTEM_SOCKET.to_owned());
    let client = Docker::connect_with_socket(&host, 120, bollard::API_DEFAULT_VERSION)
        .unwrap_or_else(|err| panic!("failed to connect to the docker daemon at {host}: {err}"));
    DockerDaemon { client, host }
}

const SYSTEM_SOCKET: &str = "unix:///var/run/docker.sock";

/// rootless docker's per-user socket, if one exists
fn rootless_socket() -> Option<String> {
    let xdg = std::env::var("XDG_RUNTIME_DIR").ok()?;
    let path = format!("{xdg}/docker.sock");
    std::path::Path::new(&path)
        .exists()
        .then(|| format!("unix://{path}"))
}
