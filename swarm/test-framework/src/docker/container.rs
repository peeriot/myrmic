use std::{ops::Deref, path::Path, sync::Arc};

use bollard::{
    Docker,
    container::LogOutput,
    exec::{CreateExecOptions, StartExecResults},
    models::{
        ContainerCreateBody, ContainerUpdateBody, EndpointSettings, HostConfig, Mount, MountType,
        NetworkConnectRequest, NetworkingConfig,
    },
    query_parameters::{CreateContainerOptionsBuilder, StartContainerOptions},
};
use futures::StreamExt;

use crate::{
    CommandOutput,
    docker::{Daemon, Image, ManagedNetwork},
    run_cleanup,
};

#[derive(Clone)]
/// A running docker container, for reading its address and networks and running commands in it.
pub struct ConnectedContainer {
    docker: Daemon,
    container_id: String,
}

impl Deref for ConnectedContainer {
    type Target = Docker;

    fn deref(&self) -> &Self::Target {
        &self.docker
    }
}

impl ConnectedContainer {
    /// The running container `container_id` on `docker`.
    pub fn attach(docker: Daemon, container_id: impl Into<String>) -> Self {
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
    pub fn get_id(&self) -> &str {
        &self.container_id
    }

    /// The IP address of this container on the network `network`.
    ///
    /// This is the address docker handed out, read back from docker's inspect API. A test that
    /// changes the address inside the container's network namespace keeps seeing the old address
    /// here, so it has to read the address before such a change.
    pub async fn get_container_ip(&self, network: &str) -> String {
        let id = &self.container_id;
        let inspect = self
            .docker
            .inspect_container(id, None)
            .await
            .unwrap_or_else(|err| panic!("failed to inspect container {id}: {err}"));
        inspect
            .network_settings
            .and_then(|settings| settings.networks)
            .unwrap_or_else(|| panic!("docker reports no networks for container {id}"))
            .get(network)
            .unwrap_or_else(|| panic!("container {id} is not on network {network}"))
            .ip_address
            .clone()
            .unwrap_or_else(|| panic!("container {id} has no address on network {network}"))
    }

    /// Run `cmd` in the container and capture its output. Panics if docker can't run it: that means
    /// the test infrastructure is broken, which is not a result.
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

    /// Limit the memory of everything in the container to `bytes`, without swap on top: once its
    /// processes use more and the kernel can't reclaim any, it kills one of them.
    pub async fn set_memory_limit(&self, bytes: u64) {
        let bytes = i64::try_from(bytes).expect("a memory limit fits docker's i64");
        let id = &self.container_id;
        self.docker
            .update_container(
                id,
                ContainerUpdateBody {
                    memory: Some(bytes),
                    // the limit of memory and swap together
                    memory_swap: Some(bytes),
                    ..Default::default()
                },
            )
            .await
            .unwrap_or_else(|err| panic!("failed to limit the memory of container {id}: {err}"));
    }

    /// Connect the container to the network `network`.
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
            .unwrap_or_else(|err| {
                panic!(
                    "failed to connect container {} to network {network}: {err}",
                    self.container_id
                )
            });
    }
}

/// A docker container that this crate started. It is force-removed when dropped, so cleanup also
/// happens when the test panics.
///
/// It isn't `Clone`: exactly one value owns the container. To work with the container, borrow it
/// (it dereferences to [`ConnectedContainer`]), or share it in an `Arc`, as a
/// [`crate::host::Docker`] does.
pub struct ManagedContainer {
    container: ConnectedContainer,
    /// Keeps the network the container is attached to alive. Fields are dropped after
    /// [`Drop::drop`], so the network is only released once the container is removed.
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
        docker: Daemon,
        name: &'a str,
    ) -> ContainerBuilder<'a> {
        ContainerBuilder {
            image,
            docker,
            name,
            command: &[],
            capabilities: &[],
            tmpfs: vec![],
            volumes: vec![],
            network: None,
        }
    }
}

/// Options for one docker container, created by [`ManagedContainer::builder`].
pub(crate) struct ContainerBuilder<'a> {
    image: &'a Image,
    docker: Daemon,
    name: &'a str,
    command: &'a [&'a str],
    capabilities: &'a [&'a str],
    tmpfs: Vec<(&'a str, &'a str)>,
    volumes: Vec<&'a str>,
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

    /// A docker volume of its own mounted at `path`: a directory on the docker host's disk, removed
    /// together with the container.
    pub(crate) fn volume(mut self, path: &'a str) -> Self {
        self.volumes.push(path);
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
        let name = self.name;
        let options = CreateContainerOptionsBuilder::default().name(name).build();
        let created = self
            .docker
            .create_container(Some(options), self.create_body())
            .await
            .unwrap_or_else(|err| panic!("failed to create container {name}: {err}"));
        // owned from here on, so a failure below still removes the container
        let managed = ManagedContainer {
            container: ConnectedContainer::attach(self.docker, created.id),
            _network: self.network,
        };

        managed
            .start_container(managed.get_id(), None::<StartContainerOptions>)
            .await
            .unwrap_or_else(|err| panic!("failed to start container {name}: {err}"));

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
            image: Some(self.image.get_tag().to_owned()),
            cmd: (!self.command.is_empty()).then(|| {
                self.command
                    .iter()
                    .map(|arg| (*arg).to_owned())
                    .collect::<Vec<_>>()
            }),
            env,
            networking_config: self.network.as_deref().map(|network| NetworkingConfig {
                endpoints_config: Some(std::collections::HashMap::from([(
                    network.get_name().to_owned(),
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
                // without a source, docker creates an anonymous volume for the container
                mounts: Some(
                    self.volumes
                        .iter()
                        .map(|path| Mount {
                            target: Some((*path).to_owned()),
                            typ: Some(MountType::VOLUME),
                            ..Default::default()
                        })
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
        // `-v`: with its anonymous volumes ([`ContainerBuilder::volume`])
        rm.args(["rm", "-f", "-v", self.container.get_id()]);
        run_cleanup("ManagedContainer", rm);
    }
}
