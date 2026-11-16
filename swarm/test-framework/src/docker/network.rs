use std::net::IpAddr;

use bollard::models::NetworkCreateRequest;

use super::{ConnectedContainer, Daemon};
use crate::run_cleanup;

/// a docker network that was created by this framework and is removed when dropped
///
/// The [`ManagedContainer`](super::ManagedContainer)s attached to it hold it, so it
/// outlives them.
pub struct ManagedNetwork {
    docker: Daemon,
    name: String,
    /// a running container this framework did not create (e.g. the one the test runs in),
    /// attached at creation and detached before removal: docker refuses to remove a network
    /// that has a container attached
    outsider: Option<String>,
}

impl ManagedNetwork {
    /// create a bridge network named `name`, with the running container `outsider` attached
    pub(crate) async fn create(docker: Daemon, name: &str, outsider: Option<&str>) -> Self {
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

    /// The `IPv4` address the machine that created the network has on it: the outsider's if one
    /// is attached (see [`Self::create`]), otherwise the docker host's, the network's gateway.
    pub(crate) async fn get_creator_address(&self) -> IpAddr {
        let address = match &self.outsider {
            Some(outsider) => self
                .docker
                .inspect_container(outsider, None)
                .await
                .unwrap_or_else(|err| {
                    panic!("failed to inspect docker container `{outsider}`: {err}")
                })
                .network_settings
                .and_then(|settings| settings.networks)
                .and_then(|mut networks| networks.remove(&self.name))
                .and_then(|endpoint| endpoint.ip_address)
                .unwrap_or_else(|| {
                    panic!(
                        "docker gave `{outsider}` no address on network `{}`",
                        self.name
                    )
                }),
            None => self
                .docker
                .inspect_network(&self.name, None)
                .await
                .unwrap_or_else(|err| {
                    panic!("failed to inspect docker network `{}`: {err}", self.name)
                })
                .ipam
                .and_then(|ipam| ipam.config)
                .into_iter()
                .flatten()
                .find_map(|config| config.gateway.filter(|gateway| gateway.contains('.')))
                .unwrap_or_else(|| panic!("docker network `{}` has no IPv4 gateway", self.name)),
        };
        address.parse().unwrap_or_else(|err| {
            panic!("docker gave the address `{address}`, which is none: {err}")
        })
    }

    /// the prefix length of the network's `IPv4` subnet, e.g. 16 for `172.19.0.0/16`
    pub async fn ipv4_prefix_len(&self) -> u8 {
        let inspect = self
            .docker
            .inspect_network(&self.name, None)
            .await
            .unwrap_or_else(|err| {
                panic!("failed to inspect docker network `{}`: {err}", self.name)
            });
        let subnet = inspect
            .ipam
            .and_then(|ipam| ipam.config)
            .into_iter()
            .flatten()
            .find_map(|config| config.subnet.filter(|subnet| subnet.contains('.')))
            .unwrap_or_else(|| panic!("docker network `{}` has no IPv4 subnet", self.name));
        let (_, len) = subnet
            .split_once('/')
            .unwrap_or_else(|| panic!("docker gave the subnet `{subnet}` without a prefix length"));
        len.parse().unwrap_or_else(|err| {
            panic!("docker gave the subnet `{subnet}` a bad prefix length: {err}")
        })
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
