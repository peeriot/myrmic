use std::ops::Deref;
use std::path::Path;
use std::sync::Arc;

use bollard::{
    Docker,
    models::{
        ContainerCreateBody, EndpointSettings, HostConfig, NetworkConnectRequest, NetworkingConfig,
    },
    query_parameters::{CreateContainerOptionsBuilder, StartContainerOptions},
};

use crate::docker::{DockerDaemon, image::Image, network::ManagedNetwork, run_cleanup};

#[derive(Clone)]
/// a thin wrapper for interacting with a running docker container (exec, logs, network shaping)
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
    /// run the docker image `image` with command, attached to `network` if given
    pub(crate) async fn run(
        image: &Image,
        docker: DockerDaemon,
        command: &[&str],
        network: Option<Arc<ManagedNetwork>>,
        name: &str,
    ) -> Self {
        let options = CreateContainerOptionsBuilder::default().name(name).build();
        let created = docker
            .create_container(
                Some(options),
                container_config_with_command(
                    image.tag().to_owned(),
                    command,
                    network.as_deref().map(ManagedNetwork::name),
                ),
            )
            .await
            .unwrap();
        // owned from here on, so a failure below still removes the container
        let managed = Self {
            container: ConnectedContainer::attach(docker, created.id),
            _network: network,
        };

        managed
            .start_container(managed.id(), None::<StartContainerOptions>)
            .await
            .unwrap();

        managed
    }
}

impl Drop for ManagedContainer {
    fn drop(&mut self) {
        let mut rm = self.container.docker_cli();
        rm.args(["rm", "-f", self.container.id()]);
        run_cleanup("ManagedContainer", rm);
    }
}

fn container_config_with_command(
    image: String,
    command: &[&str],
    network: Option<&str>,
) -> ContainerCreateBody {
    let mut env = None;
    let mut host_config = None;

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
        host_config = Some(HostConfig {
            binds: Some(vec![format!("{host_dir}:/coverage")]),
            ..Default::default()
        });
    }

    ContainerCreateBody {
        image: Some(image),
        cmd: (!command.is_empty()).then(|| {
            command
                .iter()
                .map(|arg| (*arg).to_owned())
                .collect::<Vec<_>>()
        }),
        env,
        networking_config: network.map(|name| NetworkingConfig {
            endpoints_config: Some(std::collections::HashMap::from([(
                name.to_owned(),
                EndpointSettings::default(),
            )])),
        }),
        host_config,
        ..Default::default()
    }
}
