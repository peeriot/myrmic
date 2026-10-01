use std::{ops::Deref, path::Path};

use bollard::{
    models::{ContainerCreateBody, EndpointSettings, HostConfig, NetworkingConfig},
    query_parameters::{CreateContainerOptionsBuilder, StartContainerOptions},
};

use crate::docker::{DockerDaemon, container::ConnectedContainer, image::Image};

/// a [`ManagedContainer`] is a docker container that was started by this framework and is
/// force-removed when dropped (panic-safe cleanup)
///
/// Not `Clone`: exactly one value owns the container. Borrow it (it derefs to
/// [`ConnectedContainer`]) to work with it, e.g. `Myrmic::attach(&container)`.
pub struct ManagedContainer {
    container: ConnectedContainer,
}

impl Deref for ManagedContainer {
    type Target = ConnectedContainer;

    fn deref(&self) -> &Self::Target {
        &self.container
    }
}

impl ManagedContainer {
    /// run a docker image with command and connect to networks
    pub(crate) async fn run(
        image: &Image,
        docker: DockerDaemon,
        command: &[&str],
        networks: &[&str],
        name: &str,
    ) -> Self {
        let options = CreateContainerOptionsBuilder::default().name(name).build();
        let first_network = networks.first().copied();
        let created = docker
            .create_container(
                Some(options),
                container_config_with_command(image.tag().into(), command, first_network),
            )
            .await
            .unwrap();
        // owned from here on, so a failure below still removes the container
        let managed = Self {
            container: ConnectedContainer::attach(docker, created.id),
        };

        managed
            .start_container(managed.id(), None::<StartContainerOptions>)
            .await
            .unwrap();

        for network in networks.iter().skip(1) {
            managed.connect_network(network).await;
        }

        managed
    }
}

impl Drop for ManagedContainer {
    fn drop(&mut self) {
        let result = self
            .container
            .docker_cli()
            .args(["rm", "-f", self.container.id()])
            .output();
        match result {
            Ok(output) if output.status.success() => {}
            Ok(output) => eprintln!(
                "ManagedContainer drop-guard: docker rm -f {} failed: {}",
                self.container.id(),
                String::from_utf8_lossy(&output.stderr)
            ),
            Err(err) => eprintln!("ManagedContainer drop-guard: failed to run docker: {err}"),
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
