use std::{ops::Deref, path::Path};

use bollard::{
    Docker,
    models::{
        ContainerCreateBody, EndpointSettings, HostConfig, NetworkConnectRequest, NetworkingConfig,
    },
    query_parameters::{
        CreateContainerOptionsBuilder, RemoveContainerOptionsBuilder, RemoveImageOptionsBuilder,
        StartContainerOptions, StopContainerOptions,
    },
};

use crate::docker::{container::ConnectedContainer, image::Image};

/// what [`ManagedContainer::cleanup`] should tear down
#[derive(Clone, Default)]
pub struct CleanupOptions {
    /// stop and force-remove the container
    pub stop_container: bool,
    /// additionally force-remove the image with this tag
    pub remove_image: Option<String>,
}

#[derive(Clone)]
/// a [`ManagedContainer`] is a docker container that was started using this framework and can have
/// specific cleanup options.
pub struct ManagedContainer {
    container: ConnectedContainer,
    /// what [`Self::cleanup`] tears down
    pub cleanup_opts: CleanupOptions,
}

impl Deref for ManagedContainer {
    type Target = ConnectedContainer;

    fn deref(&self) -> &Self::Target {
        &self.container
    }
}

impl From<ConnectedContainer> for ManagedContainer {
    fn from(value: ConnectedContainer) -> Self {
        Self {
            container: value,
            cleanup_opts: CleanupOptions::default(),
        }
    }
}

impl ManagedContainer {
    /// as [`ConnectedContainer`] also a [`ManagedContainer`] can be attached to a running container id
    pub fn attach(docker: Docker, container_id: impl Into<String>) -> Self {
        Self {
            container: ConnectedContainer::attach(docker, container_id),
            cleanup_opts: CleanupOptions::default(),
        }
    }

    /// run a docker image with command and connect to networks
    pub(crate) async fn run(
        image: &Image,
        docker: Docker,
        command: &[&str],
        networks: &[&str],
        name: &str,
    ) -> Self {
        let options = CreateContainerOptionsBuilder::default().name(name).build();
        let first_network = networks.first().copied();
        let container = docker
            .create_container(
                Some(options),
                container_config_with_command(image.tag().into(), command, first_network),
            )
            .await
            .unwrap();

        docker
            .start_container(&container.id, None::<StartContainerOptions>)
            .await
            .unwrap();

        for network in networks.iter().skip(1) {
            docker
                .connect_network(
                    network,
                    NetworkConnectRequest {
                        container: container.id.clone(),
                        endpoint_config: Some(EndpointSettings::default()),
                    },
                )
                .await
                .unwrap();
        }

        Self {
            container: ConnectedContainer::attach(docker, container.id),
            cleanup_opts: CleanupOptions {
                stop_container: true,
                ..Default::default()
            },
        }
    }

    /// depending on clean up options, stop the container or even delete the image
    pub async fn cleanup(&self) {
        if self.cleanup_opts.stop_container {
            self.container
                .stop_container(self.container.id(), None::<StopContainerOptions>)
                .await
                .unwrap();
            self.container
                .remove_container(
                    self.container.id(),
                    Some(RemoveContainerOptionsBuilder::default().force(true).build()),
                )
                .await
                .unwrap();
        }

        if let Some(image) = &self.cleanup_opts.remove_image {
            self.container
                .remove_image(
                    image,
                    Some(RemoveImageOptionsBuilder::default().force(true).build()),
                    None,
                )
                .await
                .unwrap();
        }
    }
}

fn container_config_with_command(
    image: String,
    command: &[&str],
    first_network: Option<&str>,
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
        networking_config: first_network.map(|name| NetworkingConfig {
            endpoints_config: Some(std::collections::HashMap::from([(
                name.to_owned(),
                EndpointSettings::default(),
            )])),
        }),
        host_config,
        ..Default::default()
    }
}
