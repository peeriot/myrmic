use std::{ops::Deref, path::Path, sync::Arc};

use bollard::{
    models::{
        ContainerCreateBody, EndpointSettings, HostConfig, NetworkCreateRequest, NetworkingConfig,
    },
    query_parameters::{CreateContainerOptionsBuilder, StartContainerOptions},
};

use crate::docker::{DockerDaemon, container::ConnectedContainer, image::Image};

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
    /// run a docker image with command, attached to `network` if given
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
                    image.tag().into(),
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

/// a docker network that was created by this framework and is removed when dropped
///
/// The [`ManagedContainer`]s attached to it hold it, so it outlives them.
pub struct ManagedNetwork {
    docker: DockerDaemon,
    name: String,
    /// a running container this framework did not create (e.g. the one the test runs in),
    /// attached at creation and detached before removal: docker refuses to remove a network
    /// that has a container attached
    outsider: Option<String>,
}

impl ManagedNetwork {
    /// create a bridge network named `name`, with the running container `outsider` attached
    pub(crate) async fn create(docker: DockerDaemon, name: &str, outsider: Option<&str>) -> Self {
        docker
            .create_network(NetworkCreateRequest {
                name: name.to_owned(),
                ..Default::default()
            })
            .await
            .unwrap_or_else(|err| panic!("failed to create docker network `{name}`: {err}"));
        // owned from here on, so a failure below still removes the network
        let mut network = Self {
            docker,
            name: name.to_owned(),
            outsider: None,
        };
        if let Some(outsider) = outsider {
            ConnectedContainer::attach(network.docker.clone(), outsider)
                .connect_network(name)
                .await;
            network.outsider = Some(outsider.to_owned());
        }
        network
    }

    /// the network's name
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for ManagedNetwork {
    fn drop(&mut self) {
        if let Some(outsider) = &self.outsider {
            let mut disconnect = self.docker.cli();
            disconnect.args(["network", "disconnect", &self.name, outsider]);
            run_cleanup("ManagedNetwork", disconnect);
        }
        let mut rm = self.docker.cli();
        rm.args(["network", "rm", &self.name]);
        run_cleanup("ManagedNetwork", rm);
    }
}

/// Run the docker `command` of the drop-guard `guard`. A failure is reported, not raised: a panic
/// during unwinding aborts the process and hides the original one.
fn run_cleanup(guard: &str, mut command: std::process::Command) {
    let invocation = format!("{command:?}");
    match command.output() {
        Ok(output) if output.status.success() => {}
        Ok(output) => eprintln!(
            "{guard} drop-guard: {invocation} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(err) => eprintln!("{guard} drop-guard: failed to run {invocation}: {err}"),
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
