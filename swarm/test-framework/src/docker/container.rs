use std::ops::Deref;
use std::path::Path;
use std::sync::Arc;

use bollard::{
    Docker,
    container::LogOutput,
    exec::{CreateExecOptions, StartExecResults},
    models::{
        ContainerCreateBody, EndpointSettings, HostConfig, NetworkConnectRequest, NetworkingConfig,
    },
    query_parameters::{CreateContainerOptionsBuilder, StartContainerOptions},
};
use futures::StreamExt;

use crate::CommandOutput;
use crate::docker::{DockerDaemon, image::Image, network::ManagedNetwork, run_cleanup};

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

impl ManagedContainer {
    /// describe a container `name` of the docker image `image` on `docker`; finish with
    /// [`ContainerBuilder::run`]
    pub(crate) fn builder<'a>(
        image: &'a Image,
        docker: DockerDaemon,
        name: &'a str,
    ) -> ContainerBuilder<'a> {
        ContainerBuilder {
            image,
            docker,
            name,
            command: &[],
            capabilities: &[],
            tmpfs: vec![],
            network: None,
        }
    }
}

/// Options for one docker container, created by [`ManagedContainer::builder`].
pub(crate) struct ContainerBuilder<'a> {
    image: &'a Image,
    docker: DockerDaemon,
    name: &'a str,
    command: &'a [&'a str],
    capabilities: &'a [&'a str],
    tmpfs: Vec<(&'a str, &'a str)>,
    network: Option<Arc<ManagedNetwork>>,
}

impl<'a> ContainerBuilder<'a> {
    /// the command the container runs; defaults to the image's
    pub(crate) fn command(mut self, command: &'a [&'a str]) -> Self {
        self.command = command;
        self
    }

    /// extra Linux capabilities, e.g. `NET_ADMIN`
    pub(crate) fn capabilities(mut self, capabilities: &'a [&'a str]) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// a tmpfs mounted at `path`, with the mount `options` (e.g. `size=64m`)
    pub(crate) fn tmpfs(mut self, path: &'a str, options: &'a str) -> Self {
        self.tmpfs.push((path, options));
        self
    }

    /// the network the container is attached to; it stays alive at least as long as the
    /// container
    pub(crate) fn network(mut self, network: Arc<ManagedNetwork>) -> Self {
        self.network = Some(network);
        self
    }

    /// Create and start the container.
    pub(crate) async fn run(self) -> ManagedContainer {
        let options = CreateContainerOptionsBuilder::default()
            .name(self.name)
            .build();
        let created = self
            .docker
            .create_container(Some(options), self.create_body())
            .await
            .unwrap();
        // owned from here on, so a failure below still removes the container
        let managed = ManagedContainer {
            container: ConnectedContainer::attach(self.docker, created.id),
            _network: self.network,
        };

        managed
            .start_container(managed.id(), None::<StartContainerOptions>)
            .await
            .unwrap();

        managed
    }

    /// the container's settings for docker's create API
    fn create_body(&self) -> ContainerCreateBody {
        let mut env = None;
        let mut binds = None;

        if let Ok(profile_file) = std::env::var("LLVM_PROFILE_FILE") {
            let profile_path = Path::new(&profile_file);
            let host_dir = profile_path
                .parent()
                .map_or_else(|| ".".to_owned(), |p| p.to_string_lossy().into_owned());
            let filename = profile_path.file_name().map_or_else(
                || "default.profraw".to_owned(),
                |f| f.to_string_lossy().into_owned(),
            );
            env = Some(vec![format!("LLVM_PROFILE_FILE=/coverage/{filename}")]);
            binds = Some(vec![format!("{host_dir}:/coverage")]);
        }

        ContainerCreateBody {
            image: Some(self.image.tag().to_owned()),
            cmd: (!self.command.is_empty()).then(|| {
                self.command
                    .iter()
                    .map(|arg| (*arg).to_owned())
                    .collect::<Vec<_>>()
            }),
            env,
            networking_config: self.network.as_deref().map(|network| NetworkingConfig {
                endpoints_config: Some(std::collections::HashMap::from([(
                    network.name().to_owned(),
                    EndpointSettings::default(),
                )])),
            }),
            host_config: Some(HostConfig {
                binds,
                cap_add: Some(
                    self.capabilities
                        .iter()
                        .map(|capability| (*capability).to_owned())
                        .collect(),
                ),
                tmpfs: Some(
                    self.tmpfs
                        .iter()
                        .map(|(path, options)| ((*path).to_owned(), (*options).to_owned()))
                        .collect(),
                ),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

impl Drop for ManagedContainer {
    fn drop(&mut self) {
        let mut rm = self.container.docker_cli();
        rm.args(["rm", "-f", self.container.id()]);
        run_cleanup("ManagedContainer", rm);
    }
}
