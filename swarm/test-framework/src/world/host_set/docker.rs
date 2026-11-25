use std::{path::Path, sync::Arc};

use sha2::{Digest, Sha256};
use tokio::sync::OnceCell;

use zenoh::config::EndPoint;

use super::{ConnectedHostSet, route_by_address, sealed};
use crate::{docker, host, resolve_binary};

/// A docker network of its own. Its hosts are containers ([`crate::host::Docker`]) and the test's
/// own machine. The hosts dial each other at their addresses on the network, which a fault can
/// change ([`host::Docker::readdress`]), and the network carries multicast.
///
/// The test reaches the network's hosts at their addresses on it. When the test runs directly on a
/// machine, that machine is on every docker network. When the test runs in a container itself (e.g.
/// a CI job's container), it isn't. Then the test names its container in [`TEST_CONTAINER`], and
/// the network attaches that container.
#[derive(Clone)]
pub struct Docker {
    shared: Arc<DockerShared>,
}

/// What the clones of a [`Docker`] host set share.
struct DockerShared {
    daemon: docker::Daemon,
    network: Arc<docker::ManagedNetwork>,
    image: &'static docker::Image,
}

/// The environment variable naming the container the test runs in, if it runs in one.
pub const TEST_CONTAINER: &str = "MYRMIC_TEST_CONTAINER";

/// The image's Dockerfile; its binaries come from the workspace build.
const DOCKERFILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/docker/Dockerfile");

impl Docker {
    /// A new docker network on `daemon`, with no host yet. Builds the hosts' image first unless
    /// the daemon has it.
    pub async fn new(daemon: &docker::Daemon) -> Self {
        let image = ensure_image(daemon).await;
        let test_container = match std::env::var(TEST_CONTAINER) {
            Ok(id) => Some(id),
            // the test runs on the docker host
            Err(std::env::VarError::NotPresent) => None,
            Err(err) => panic!("invalid ${TEST_CONTAINER}: {err}"),
        };
        let name = format!("myrmic-net-{}", uuid::Uuid::new_v4().simple());
        let network =
            docker::ManagedNetwork::create(daemon.clone(), &name, test_container.as_deref()).await;
        // the test's machine is where the test runs, the container it names or the docker host
        host::set_local_address(&name, network.get_creator_address().await);
        Self {
            shared: Arc::new(DockerShared {
                daemon: daemon.clone(),
                network: Arc::new(network),
                image,
            }),
        }
    }

    /// A new host on this network: a container of its own, which runs nothing until a shim or a
    /// router runs something in it.
    pub async fn new_host(&self) -> host::Docker {
        let DockerShared {
            daemon,
            network,
            image,
        } = &*self.shared;
        let name = format!("{}-{}", network.get_name(), uuid::Uuid::new_v4().simple());
        // `NET_ADMIN` for the nftables rules of the faults
        let container = docker::ManagedContainer::builder(image, daemon.clone(), &name)
            .command(&["sleep", "infinity"])
            .capabilities(&["NET_ADMIN"])
            .tmpfs(host::DOCKER_DATA_DIR, host::DOCKER_DATA_DIR_OPTIONS)
            .network(network.clone())
            .run()
            .await;
        let address = container
            .get_container_ip(network.get_name())
            .await
            .parse()
            .expect("docker reports a host's address as an IP address");
        host::Docker::new(Arc::new(container), network.clone(), address)
    }
}

impl ConnectedHostSet for Docker {
    fn has_host(&self, host: &host::Id) -> bool {
        host.has_address(self.shared.network.get_name())
    }

    fn has_multicast(&self) -> bool {
        true
    }

    fn find_route(
        &self,
        from: &host::Id,
        to: &host::Id,
        port: u16,
        _: sealed::Token,
    ) -> Option<EndPoint> {
        Some(route_by_address(
            self.shared.network.get_name(),
            from,
            to,
            port,
        ))
    }
}

impl Drop for DockerShared {
    fn drop(&mut self) {
        host::remove_local_address(self.network.get_name());
    }
}

/// Build the image with the workspace's current binaries, unless the daemon already has it. This
/// happens once per process: all networks of one test binary share the image.
///
/// The binaries are stripped, and the image is tagged with a hash of them and of the Dockerfile. So
/// the image is built once per build of the binaries, and every later run reuses it, in any
/// process. Test processes that start side by side take turns on a lock file named after the tag:
/// the first builds the image, the others then find it, instead of each building it again (and
/// docker refusing all but one of the tags).
async fn ensure_image(daemon: &docker::Daemon) -> &'static docker::Image {
    static IMAGE: OnceCell<docker::Image> = OnceCell::const_new();
    IMAGE
        .get_or_init(|| async {
            let stripped =
                tempfile::tempdir().expect("failed to create a directory for the binaries");
            let myrmic = stripped.path().join("myrmic");
            let router = stripped.path().join("zenoh-router");
            strip(&resolve_binary!("myrmic"), &myrmic).await;
            strip(&resolve_binary!("zenoh-router"), &router).await;

            let mut hash = Sha256::new();
            for file in [Path::new(DOCKERFILE), &myrmic, &router] {
                hash.update(
                    std::fs::read(file)
                        .unwrap_or_else(|err| panic!("failed to read {}: {err}", file.display())),
                );
            }
            let hash = &hex::encode(hash.finalize())[..16];
            let tag = format!("myrmic-swarm:{hash}");

            let lock_path = std::env::temp_dir().join(format!("myrmic-swarm-{hash}.lock"));
            // held until the image is there; waiting for it blocks the thread, for as long as
            // another process builds
            let _lock = tokio::task::spawn_blocking(move || {
                let lock = std::fs::File::create(&lock_path).unwrap_or_else(|err| {
                    panic!("failed to create {}: {err}", lock_path.display())
                });
                lock.lock()
                    .unwrap_or_else(|err| panic!("failed to lock {}: {err}", lock_path.display()));
                lock
            })
            .await
            .unwrap_or_else(|err| std::panic::resume_unwind(err.into_panic()));
            match docker::Image::find(daemon, &tag).await {
                Some(image) => image,
                None => {
                    docker::Image::build(
                        daemon,
                        tag,
                        Path::new(DOCKERFILE),
                        &[(&myrmic, "myrmic"), (&router, "zenoh-router")],
                    )
                    .await
                }
            }
        })
        .await
}

/// Copy `binary` to `stripped` without its debug info: the debug build's binaries are mostly
/// debug info, which would make every image build upload several GB.
async fn strip(binary: &Path, stripped: &Path) {
    let status = tokio::process::Command::new("strip")
        .args(["--strip-debug", "-o"])
        .arg(stripped)
        .arg(binary)
        .status()
        .await
        .expect("failed to run strip");
    assert!(
        status.success(),
        "strip of {} failed: {status}",
        binary.display()
    );
}
