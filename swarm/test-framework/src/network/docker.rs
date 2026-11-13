use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    path::Path,
    sync::{Arc, Mutex},
};

use sha2::{Digest, Sha256};
use tokio::sync::OnceCell;

use super::{HostId, Network};
use crate::{
    docker::{DockerDaemon, container::ManagedContainer, image::Image, network::ManagedNetwork},
    host, resolve_binary,
};

/// A docker network of its own, whose hosts are containers ([`crate::host::Docker`]). The test
/// cuts its hosts apart ([`Self::isolate`], [`Self::block`]) or moves one ([`Self::readdress`]),
/// and answers [`Network::reaches`] by the cuts it has in place, [`Network::get_address`] by
/// where it moved the hosts.
///
/// The test reaches the network's hosts at their addresses on it. On a machine that runs the test
/// directly, that machine is on every docker network. A test that runs in a container itself
/// (e.g. a CI job container) is not, so it names that container in [`TEST_CONTAINER`], and the
/// network attaches it.
///
/// The faults are nftables rules in the hosts' containers, so they act on everything in a
/// container but its loopback traffic: what runs on one host always reaches itself.
#[derive(Clone)]
pub struct Docker {
    shared: Arc<DockerShared>,
}

/// What the clones of a [`Docker`] network share.
struct DockerShared {
    daemon: DockerDaemon,
    network: Arc<ManagedNetwork>,
    image: &'static Image,
    /// the address each host has now
    addresses: Mutex<HashMap<HostId, IpAddr>>,
    cuts: Mutex<Cuts>,
}

/// The cuts a [`Docker`] network has in place.
#[derive(Default)]
struct Cuts {
    /// hosts cut off from everything ([`Docker::isolate`])
    isolated: HashSet<HostId>,
    /// pairs of hosts cut from each other ([`Docker::block`]), the lower id first
    blocked: HashSet<(HostId, HostId)>,
}

/// The environment variable naming the container the test runs in, if it runs in one.
pub const TEST_CONTAINER: &str = "MYRMIC_TEST_CONTAINER";

/// The image's Dockerfile; its binaries come from the workspace build.
const DOCKERFILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/docker/Dockerfile");

/// The nftables commands of [`Docker::isolate`]: drop everything in and out of the container but
/// its loopback traffic. Its own table, so [`Docker::heal`] removes exactly these rules.
const ISOLATE_RULES: &str = "add table inet isolate; \
    add chain inet isolate input { type filter hook input priority 0; policy drop; }; \
    add chain inet isolate output { type filter hook output priority 0; policy drop; }; \
    add rule inet isolate input iif lo accept; \
    add rule inet isolate output oif lo accept";

/// The nftables table of [`Docker::block`]: its rules drop the traffic to and from single
/// addresses, and let everything else through. `add` is a no-op for a table or chain that exists
/// already.
const BLOCK_TABLE: &str = "add table inet block; \
    add chain inet block input { type filter hook input priority 0; policy accept; }; \
    add chain inet block output { type filter hook output priority 0; policy accept; }";

/// The last octet of the address [`Docker::readdress`] moves a host to: far above the addresses
/// docker hands out in order, so no other container has it.
const READDRESSED_HOST: u8 = 200;

impl Docker {
    /// A new docker network on `daemon`, with no host yet. Builds the hosts' image first unless
    /// the daemon has it.
    pub async fn new(daemon: &DockerDaemon) -> Self {
        let image = ensure_image(daemon).await;
        let test_container = match std::env::var(TEST_CONTAINER) {
            Ok(id) => Some(id),
            // the test runs on the docker host
            Err(std::env::VarError::NotPresent) => None,
            Err(err) => panic!("invalid ${TEST_CONTAINER}: {err}"),
        };
        let name = format!("myrmic-net-{}", uuid::Uuid::new_v4().simple());
        let network =
            ManagedNetwork::create(daemon.clone(), &name, test_container.as_deref()).await;
        // the test's machine is where the test runs, the container it names or the docker host
        let test_machine = network.get_creator_address().await;
        Self {
            shared: Arc::new(DockerShared {
                daemon: daemon.clone(),
                network: Arc::new(network),
                image,
                addresses: Mutex::new(HashMap::from([(HostId::TEST_MACHINE, test_machine)])),
                cuts: Mutex::new(Cuts::default()),
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
            ..
        } = &*self.shared;
        let name = format!("{}-{}", network.name(), uuid::Uuid::new_v4().simple());
        // `NET_ADMIN` for the nftables rules of the faults
        let container = ManagedContainer::builder(image, daemon.clone(), &name)
            .command(&["sleep", "infinity"])
            .capabilities(&["NET_ADMIN"])
            .tmpfs(host::DOCKER_DATA_DIR, host::DOCKER_DATA_DIR_OPTIONS)
            .network(network.clone())
            .run()
            .await;
        let address = container
            .container_ip(network.name())
            .await
            .parse()
            .expect("docker reports a host's address as an IP address");
        let id = HostId::new();
        self.lock_addresses().insert(id, address);
        host::Docker::new(Arc::new(container), id)
    }

    /// Cut `host` off the network: drop all its traffic but its loopback, as a host whose cable is
    /// pulled. What runs on it keeps running, and the others only notice its silence.
    pub async fn isolate(&self, host: &host::Docker) {
        let output = host.get_container().exec(&["nft", ISOLATE_RULES]).await;
        assert!(
            output.success,
            "failed to isolate {}: {}",
            self.get_address(host.get_id()),
            output.stderr
        );
        self.lock_cuts().isolated.insert(host.get_id());
    }

    /// lift [`Self::isolate`] of `host`
    pub async fn heal(&self, host: &host::Docker) {
        let output = host
            .get_container()
            .exec(&["nft", "delete table inet isolate"])
            .await;
        assert!(
            output.success,
            "failed to heal {}: {}",
            self.get_address(host.get_id()),
            output.stderr
        );
        self.lock_cuts().isolated.remove(&host.get_id());
    }

    /// Drop all traffic between `a` and `b`. Both stay connected to everything else, and may still
    /// reach each other through it.
    pub async fn block(&self, a: &host::Docker, b: &host::Docker) {
        self.drop_traffic(a, b).await;
        self.drop_traffic(b, a).await;
        self.lock_cuts()
            .blocked
            .insert(ordered(a.get_id(), b.get_id()));
    }

    /// Move `host` to another address on the network, as when a host's address changes, without
    /// telling what runs on it or the others: finding back is their business. From then on the
    /// network has `host` at the new address, so what starts afterwards connects to it there,
    /// while what runs keeps the endpoints it started with. Docker isn't told, which nothing
    /// needs: the network asks docker for a host's address only when it creates the host.
    ///
    /// `host` must not be blocked from another host ([`Self::block`]): those rules drop the
    /// traffic of its old address.
    pub async fn readdress(&self, host: &host::Docker) {
        let id = host.get_id();
        assert!(
            self.lock_cuts()
                .blocked
                .iter()
                .all(|&(a, b)| a != id && b != id),
            "a blocked host can't move: its blocks name its old address"
        );
        let old = self.get_address(id).to_string();
        let prefix = self.shared.network.ipv4_prefix_len().await;
        assert!(
            prefix <= 24,
            "the last octet alone must stay within the /{prefix} subnet"
        );
        let (network_part, last) = old
            .rsplit_once('.')
            .unwrap_or_else(|| panic!("`{old}` is no IPv4 address"));
        assert_ne!(
            last,
            READDRESSED_HOST.to_string(),
            "`{old}` was moved already"
        );
        let new = format!("{network_part}.{READDRESSED_HOST}");
        // Dropping the old address also drops the on-link route the default route needs, so that
        // one is captured first and put back; its words are split again on purpose, it is a whole
        // route specification.
        let script = format!(
            "set -eu; \
             dev=$(ip -o -4 addr show | awk '$4 == \"{old}/{prefix}\" {{ print $2; exit }}'); \
             default_route=$(ip -4 route show default | head -n 1); \
             ip addr del {old}/{prefix} dev \"$dev\"; \
             ip addr add {new}/{prefix} dev \"$dev\"; \
             [ -z \"$default_route\" ] || ip -4 route replace $default_route"
        );
        let output = host.get_container().exec(&["sh", "-c", &script]).await;
        assert!(
            output.success,
            "failed to move {old} to {new}: {}",
            output.stderr
        );
        let new = new.parse().expect("the new address is an IP address");
        self.lock_addresses().insert(id, new);
    }

    /// Drop all traffic between `host` and `peer`, in `host`'s [`BLOCK_TABLE`].
    async fn drop_traffic(&self, host: &host::Docker, peer: &host::Docker) {
        let peer = self.get_address(peer.get_id());
        let rules = format!(
            "{BLOCK_TABLE}; \
             add rule inet block input ip saddr {peer} drop; \
             add rule inet block output ip daddr {peer} drop"
        );
        let output = host.get_container().exec(&["nft", &rules]).await;
        assert!(
            output.success,
            "failed to drop the traffic between {} and {peer}: {}",
            self.get_address(host.get_id()),
            output.stderr
        );
    }

    fn lock_addresses(&self) -> std::sync::MutexGuard<'_, HashMap<HostId, IpAddr>> {
        self.shared.addresses.lock().expect("not poisoned")
    }

    fn lock_cuts(&self) -> std::sync::MutexGuard<'_, Cuts> {
        self.shared.cuts.lock().expect("not poisoned")
    }
}

impl Network for Docker {
    fn reaches(&self, a: HostId, b: HostId) -> bool {
        let cuts = self.lock_cuts();
        !cuts.isolated.contains(&a)
            && !cuts.isolated.contains(&b)
            && !cuts.blocked.contains(&ordered(a, b))
    }

    fn get_address(&self, host: HostId) -> IpAddr {
        *self
            .lock_addresses()
            .get(&host)
            .unwrap_or_else(|| panic!("{host:?} is no host of this network"))
    }
}

/// `a` and `b`, the lower first: how [`Cuts::blocked`] keys a pair
fn ordered(a: HostId, b: HostId) -> (HostId, HostId) {
    (a.min(b), a.max(b))
}

/// Build the image with the workspace's current binaries, unless the daemon already has it. Once
/// per process: the networks of one test binary share it.
///
/// It strips the binaries and tags the image with a hash of them and the Dockerfile, so an image
/// is built once per build of the binaries and reused by every later run, in any process.
async fn ensure_image(daemon: &DockerDaemon) -> &'static Image {
    static IMAGE: OnceCell<Image> = OnceCell::const_new();
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
            let tag = format!("myrmic-swarm:{}", &hex::encode(hash.finalize())[..16]);

            match Image::find(daemon, &tag).await {
                Some(image) => image,
                None => {
                    Image::build(
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
