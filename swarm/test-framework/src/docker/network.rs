use bollard::models::NetworkCreateRequest;

use crate::docker::{DockerDaemon, container::ConnectedContainer, run_cleanup};

/// a docker network that was created by this framework and is removed when dropped
///
/// The [`ManagedContainer`](super::container::ManagedContainer)s attached to it hold it, so it
/// outlives them.
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
