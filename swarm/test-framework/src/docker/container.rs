use std::{ops::Deref, sync::Arc};

use bollard::{
    Docker,
    container::LogOutput,
    exec::{CreateExecOptions, StartExecResults},
    models::{EndpointSettings, NetworkConnectRequest},
};
use futures::StreamExt;

use crate::{
    CommandOutput,
    docker::{DockerDaemon, network::ManagedNetwork, run_cleanup},
};

#[derive(Clone)]
/// a thin wrapper for interacting with a running docker container (address, networks, exec)
pub struct ConnectedContainer {
    docker: DockerDaemon,
    container_id: String,
}

impl Deref for ConnectedContainer {
    type Target = Docker;

    fn deref(&self) -> &Self::Target {
        &self.docker
    }
}

impl ConnectedContainer {
    /// a [`ConnectedContainer`] is created by attaching to a `container_id`
    pub fn attach(docker: DockerDaemon, container_id: impl Into<String>) -> Self {
        Self {
            docker,
            container_id: container_id.into(),
        }
    }

    /// the `docker` CLI, pointed at the daemon this container runs on
    pub(crate) fn docker_cli(&self) -> std::process::Command {
        self.docker.cli()
    }

    /// the docker container id
    pub fn id(&self) -> &str {
        &self.container_id
    }

    /// return the IP of this container in the specified network
    ///
    /// This is the address docker handed out, read back from its inspect API. A test that
    /// changes the address inside the container's network namespace keeps seeing the old
    /// one here, so capture the address before such a change instead of asking afterwards.
    pub async fn container_ip(&self, network: &str) -> String {
        let inspect = self
            .docker
            .inspect_container(&self.container_id, None)
            .await
            .unwrap();
        inspect
            .network_settings
            .unwrap()
            .networks
            .unwrap()
            .get(network)
            .unwrap()
            .ip_address
            .clone()
            .unwrap()
    }

    /// Run `cmd` in the container and capture its output. Panics when docker cannot run it:
    /// broken test infrastructure, not a result.
    pub async fn exec(&self, cmd: &[&str]) -> CommandOutput {
        let exec = self
            .docker
            .create_exec(
                &self.container_id,
                CreateExecOptions {
                    cmd: Some(cmd.to_vec()),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap_or_else(|err| panic!("failed to create exec of {cmd:?}: {err}"));
        let StartExecResults::Attached { mut output, .. } = self
            .docker
            .start_exec(&exec.id, None)
            .await
            .unwrap_or_else(|err| panic!("failed to start exec of {cmd:?}: {err}"))
        else {
            panic!("an exec started without `detach` is attached");
        };

        let mut stdout = vec![];
        let mut stderr = vec![];
        while let Some(chunk) = output.next().await {
            match chunk.unwrap_or_else(|err| panic!("failed to read output of {cmd:?}: {err}")) {
                LogOutput::StdOut { message } => stdout.extend_from_slice(&message),
                LogOutput::StdErr { message } => stderr.extend_from_slice(&message),
                other => panic!("an exec without a tty or stdin printed {other:?}"),
            }
        }
        let inspect = self
            .docker
            .inspect_exec(&exec.id)
            .await
            .unwrap_or_else(|err| panic!("failed to inspect exec of {cmd:?}: {err}"));

        CommandOutput {
            success: inspect.exit_code == Some(0),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        }
    }

    /// connect container to a network
    pub async fn connect_network(&self, network: &str) {
        self.docker
            .connect_network(
                network,
                NetworkConnectRequest {
                    container: self.container_id.clone(),
                    endpoint_config: Some(EndpointSettings::default()),
                },
            )
            .await
            .unwrap();
    }
}

/// a [`ManagedContainer`] is a docker container that was started by this framework and is
/// force-removed when dropped (panic-safe cleanup)
///
/// Not `Clone`: exactly one value owns the container. Borrow it (it derefs to
/// [`ConnectedContainer`]) to work with it, or share it in an `Arc`, e.g.
/// `Myrmic::attach(Arc::new(container))`.
pub struct ManagedContainer {
    container: ConnectedContainer,
    /// Kept alive for the network it is attached to: fields drop after [`Drop::drop`], so the
    /// network is released only once the container is removed.
    _network: Option<Arc<ManagedNetwork>>,
}

impl Deref for ManagedContainer {
    type Target = ConnectedContainer;

    fn deref(&self) -> &Self::Target {
        &self.container
    }
}

impl Drop for ManagedContainer {
    fn drop(&mut self) {
        let mut rm = self.container.docker_cli();
        rm.args(["rm", "-f", self.container.id()]);
        run_cleanup("ManagedContainer", rm);
    }
}
