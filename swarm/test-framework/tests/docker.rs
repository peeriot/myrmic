//! Tests of [`test_framework::swarm3`] on a docker network: hosts in containers, the swarm
//! attached through a router, and the network's faults. They need a docker daemon and built
//! `myrmic` and `zenoh-router` binaries (see [`test_framework::resolve_binary!`]).

use test_framework::{
    docker::{self, DockerDaemon},
    host, myrmic, network,
    overlay::{MulticastGroup, Overlay},
    start_runtimes,
    swarm3::{self, Swarm},
};

/// a CLI runtime in a container, not started
type DockerRt = swarm3::StoppedRt<myrmic::StoppedRt<myrmic::Docker>>;

/// A swarm on a docker network of its own, attached through a router on a host of its own.
struct DockerSwarm {
    network: network::Docker,
    group: MulticastGroup,
    /// only kept alive: the swarm reaches its runtimes through it
    _router: docker::Router,
    swarm: Swarm,
}

impl DockerSwarm {
    async fn new() -> Self {
        let network = network::Docker::new(&DockerDaemon::connect()).await;
        let overlay = Overlay::new(&network);
        let group = overlay.new_multicast_group();
        let router = network.new_host().await.router(&group).await;
        let swarm = Swarm::new(overlay.connect_to(&router)).await;
        Self {
            network,
            group,
            _router: router,
            swarm,
        }
    }

    /// a CLI runtime of the swarm on a new host of the network, not started, and its host
    async fn new_runtime(&self) -> (host::Docker, DockerRt) {
        let host = self.network.new_host().await;
        let rt = self.swarm.add(host.myrmic(&self.group).runtime());
        (host, rt)
    }
}

/// A swarm attached through a router reaches its runtimes on the network's hosts, and they reach
/// each other.
#[tokio::test(flavor = "multi_thread")]
async fn a_docker_swarm_connects_through_its_router() -> anyhow::Result<()> {
    let setup = DockerSwarm::new().await;
    let (_a_host, a) = setup.new_runtime().await;
    let (_b_host, b) = setup.new_runtime().await;
    start_runtimes!(setup.swarm; a, b);
    Ok(())
}

/// A runtime whose host is cut off drops out of the swarm: it no longer answers, and the others
/// lose their links to it.
#[tokio::test(flavor = "multi_thread")]
async fn an_isolated_hosts_runtime_drops_out_of_the_swarm() -> anyhow::Result<()> {
    let setup = DockerSwarm::new().await;
    let (_a_host, a) = setup.new_runtime().await;
    let (b_host, b) = setup.new_runtime().await;
    start_runtimes!(setup.swarm; a, b);

    setup.network.isolate(&b_host).await;
    setup.swarm.wait_until_connected().await?;
    Ok(())
}

/// Two blocked hosts' runtimes lose their link to each other, and both stay in the swarm.
#[tokio::test(flavor = "multi_thread")]
async fn blocked_hosts_lose_their_link() -> anyhow::Result<()> {
    let setup = DockerSwarm::new().await;
    let (a_host, a) = setup.new_runtime().await;
    let (b_host, b) = setup.new_runtime().await;
    start_runtimes!(setup.swarm; a, b);

    setup.network.block(&a_host, &b_host).await;
    setup.swarm.wait_until_connected().await?;
    Ok(())
}
