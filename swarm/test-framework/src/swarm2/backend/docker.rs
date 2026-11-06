use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use enum_map::EnumMap;
use sha2::{Digest, Sha256};
use tokio::sync::OnceCell;

use super::{ROUTER_PORT, router_session};
use crate::{
    docker::{DockerDaemon, container::ManagedContainer, image::Image, network::ManagedNetwork},
    myrmic::{self, Myrmic},
    resolve_binary,
    swarm2::{
        Block, FillDisk, Isolate, Readdress, SkewClock, SwarmBackend, SwarmSpec, join_enum_map,
    },
};

/// [`SwarmBackend`] that runs every runtime in a docker container of its own, on a network of its
/// own, next to a container running a zenoh router. The test's CLI and session run on the test's
/// host and join the swarm through that router, which faults on runtimes never cut off.
///
/// The test reaches the router at its address on that network. On a host that runs the test
/// directly, the host is on every docker network. A test that runs in a container itself (e.g. a
/// CI job container) is not, so it names that container in [`TEST_CONTAINER`], and the backend
/// attaches it to the swarm's network.
pub struct Docker<S: SwarmSpec> {
    container_map: EnumMap<S, Arc<ManagedContainer>>,
    /// kept for the router it runs
    _router: ManagedContainer,
    /// the router's address on the swarm's network, e.g. `172.23.0.5`
    router_ip: String,
    network: Arc<ManagedNetwork>,
    /// the CLI on the test's host, connected to the router
    client: Myrmic<myrmic::Local>,
}

/// The image's Dockerfile; its binaries come from the workspace build.
const DOCKERFILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/docker/Dockerfile");

/// The environment variable naming the container the test runs in, if it runs in one.
pub const TEST_CONTAINER: &str = "MYRMIC_TEST_CONTAINER";

/// Where a runtime keeps its data in its container: the CLI's data directory as root
/// (`$HOME/.local/share/myrmic`), with the runtime's database and logs.
const DATA_DIR: &str = "/root/.local/share/myrmic";

/// The mount options of the tmpfs at [`DATA_DIR`]. Its own small file system, so [`FillDisk`]
/// fills a disk of the runtime's own instead of the host's. 128 MB is far more than a test's
/// runtime writes (a few MB), and little enough to fill in RAM. A database that a full disk
/// stopped reopens once 64 MB are free again, so the disk must be larger than that.
const DATA_DIR_OPTIONS: &str = "size=128m";

/// the file [`FillDisk::fill_disk`] fills [`DATA_DIR`] with, see [`filler`]
const FILLER: &str = "filler";

/// The nftables table of [`Block`]: its rules drop the traffic to and from single addresses, and
/// let everything else through. `add` is a no-op for a table or chain that exists already.
const BLOCK_TABLE: &str = "add table inet block; \
    add chain inet block input { type filter hook input priority 0; policy accept; }; \
    add chain inet block output { type filter hook output priority 0; policy accept; }";

/// The nftables commands of [`Isolate::isolate`]: drop everything in and out of the container
/// but its loopback traffic. Its own table, so [`Isolate::heal`] removes exactly these rules.
const ISOLATE_RULES: &str = "add table inet isolate; \
    add chain inet isolate input { type filter hook input priority 0; policy drop; }; \
    add chain inet isolate output { type filter hook output priority 0; policy drop; }; \
    add rule inet isolate input iif lo accept; \
    add rule inet isolate output oif lo accept";

impl<S: SwarmSpec> Docker<S> {
    /// Create the swarm's network, a container per runtime and the router's container.
    pub async fn init(dockerd: &DockerDaemon) -> Self {
        let image = ensure_image(dockerd).await;
        let test_container = match std::env::var(TEST_CONTAINER) {
            Ok(id) => Some(id),
            // the test runs on the docker host
            Err(std::env::VarError::NotPresent) => None,
            Err(err) => panic!("invalid ${TEST_CONTAINER}: {err}"),
        };
        let network = Arc::new(
            ManagedNetwork::create(dockerd.clone(), &uuid_name(), test_container.as_deref()).await,
        );

        let container_map = join_enum_map(|id: S| {
            let network = network.clone();
            async move {
                let name = format!("{}-{id:?}", network.name());
                // `NET_ADMIN` for the nftables rules of `Isolate`
                let container = ManagedContainer::builder(image, dockerd.clone(), &name)
                    .command(&["sleep", "infinity"])
                    .capabilities(&["NET_ADMIN"])
                    .tmpfs(DATA_DIR, DATA_DIR_OPTIONS)
                    .network(network)
                    .run()
                    .await;
                Arc::new(container)
            }
        })
        .await;
        let router_name = format!("{}-router", network.name());
        let router = ManagedContainer::builder(image, dockerd.clone(), &router_name)
            .command(&["zenoh-router"])
            .network(network.clone())
            .run()
            .await;
        let router_ip = router.container_ip(network.name()).await;

        Self {
            container_map,
            _router: router,
            client: Myrmic::local_connected(router_endpoint(&router_ip)),
            router_ip,
            network,
        }
    }
}

/// Build the image with the workspace's current binaries, unless the daemon already has it. Once
/// per process: the swarms of one test binary share it.
///
/// It strips the binaries and tags the image with a hash of them and the Dockerfile, so an image
/// is built once per build of the binaries and reused by every later run, in any process.
async fn ensure_image(dockerd: &DockerDaemon) -> &'static Image {
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

            match Image::find(dockerd, &tag).await {
                Some(image) => image,
                None => {
                    Image::build(
                        dockerd,
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

/// the path of [`FILLER`]
fn filler() -> String {
    format!("{DATA_DIR}/{FILLER}")
}

/// the zenoh endpoint of the router at `ip`
fn router_endpoint(ip: &str) -> String {
    format!("tcp/{ip}:{ROUTER_PORT}")
}

/// a name no other swarm's network or containers use
fn uuid_name() -> String {
    format!("myrmic-swarm-{}", uuid::Uuid::new_v4().simple())
}

// The bound makes `Docker<S>` `Sync`, which the `Send` future of `connect_session` borrowing it
// needs; the compiler cannot see through `S::Array` to the array it is.
impl<S: SwarmSpec> SwarmBackend<S> for Docker<S>
where
    S::Array<Arc<ManagedContainer>>: Sync,
{
    type Cli = myrmic::Docker;

    type Client = myrmic::Local;

    fn shim(&self, runtime: S) -> Myrmic<myrmic::Docker> {
        Myrmic::attach(self.container_map[runtime].clone())
    }

    fn client(&self) -> Myrmic<myrmic::Local> {
        self.client.clone()
    }

    /// a client session on the router
    async fn connect_session(&self) -> zenoh::Session {
        router_session(&router_endpoint(&self.router_ip)).await
    }
}

impl<S: SwarmSpec> Isolate<S> for Docker<S>
where
    S::Array<Arc<ManagedContainer>>: Sync,
{
    async fn isolate(&self, runtime: S) {
        let output = self.container_map[runtime]
            .exec(&["nft", ISOLATE_RULES])
            .await;
        assert!(
            output.success,
            "failed to isolate {runtime:?}: {}",
            output.stderr
        );
    }

    async fn heal(&self, runtime: S) {
        let output = self.container_map[runtime]
            .exec(&["nft", "delete table inet isolate"])
            .await;
        assert!(
            output.success,
            "failed to heal {runtime:?}: {}",
            output.stderr
        );
    }
}

impl<S: SwarmSpec> FillDisk<S> for Docker<S>
where
    S::Array<Arc<ManagedContainer>>: Sync,
{
    async fn fill_disk(&self, runtime: S, room: u64) {
        // `$1`: the data directory, `$2`: the filler, `$3`: the room
        let fill = r#"fallocate -l $(( $(df --output=avail -B1 "$1" | tail -n 1) - $3 )) "$2""#;
        let room = room.to_string();
        let output = self.container_map[runtime]
            .exec(&["sh", "-c", fill, "sh", DATA_DIR, &filler(), &room])
            .await;
        assert!(
            output.success,
            "failed to fill the disk of {runtime:?}: {}",
            output.stderr
        );
    }

    async fn free_disk(&self, runtime: S) {
        let output = self.container_map[runtime].exec(&["rm", &filler()]).await;
        assert!(
            output.success,
            "failed to free the disk of {runtime:?}: {}",
            output.stderr
        );
    }
}

impl<S: SwarmSpec> Block<S> for Docker<S>
where
    S::Array<Arc<ManagedContainer>>: Sync,
{
    async fn block(&self, a: S, b: S) {
        let a_ip = self.container_map[a]
            .container_ip(self.network.name())
            .await;
        let b_ip = self.container_map[b]
            .container_ip(self.network.name())
            .await;
        drop_traffic(&self.container_map[a], &b_ip).await;
        drop_traffic(&self.container_map[b], &a_ip).await;
    }

    async fn block_router(&self, runtime: S) {
        // in the runtime's container only: the router's has no `NET_ADMIN`, and dropping both
        // directions on one end cuts the link
        drop_traffic(&self.container_map[runtime], &self.router_ip).await;
    }
}

/// Drop all traffic between `container` and the address `ip`, in `container`'s [`BLOCK_TABLE`].
async fn drop_traffic(container: &ManagedContainer, ip: &str) {
    let rules = format!(
        "{BLOCK_TABLE}; \
         add rule inet block input ip saddr {ip} drop; \
         add rule inet block output ip daddr {ip} drop"
    );
    let output = container.exec(&["nft", &rules]).await;
    assert!(
        output.success,
        "failed to drop the traffic to and from {ip}: {}",
        output.stderr
    );
}

/// The file holding a container's clock offset in seconds, which libfaketime reads (see the
/// Dockerfile).
const FAKETIME_FILE: &str = "/etc/faketime";

/// How long a runtime may take to read a new [`FAKETIME_FILE`]: libfaketime reads it again at most
/// a second (its `FAKETIME_CACHE_DURATION`) after it last did, and once more for the processes
/// that read their clock just before the change.
const FAKETIME_PICKUP: Duration = Duration::from_secs(2);

impl<S: SwarmSpec> SkewClock<S> for Docker<S>
where
    S::Array<Arc<ManagedContainer>>: Sync,
{
    async fn skew_clock(&self, runtime: S, ahead_secs: i64) {
        // `$1`: the offset, `$2`: the file
        let write = r#"printf '%s\n' "$1" > "$2""#;
        let offset = format!("{ahead_secs:+}");
        let output = self.container_map[runtime]
            .exec(&["sh", "-c", write, "sh", &offset, FAKETIME_FILE])
            .await;
        assert!(
            output.success,
            "failed to skew the clock of {runtime:?}: {}",
            output.stderr
        );
        tokio::time::sleep(FAKETIME_PICKUP).await;
    }
}

/// The last octet of the address [`Readdress::readdress`] moves a runtime to: far above the
/// addresses docker hands out in order, so no container of the swarm has it.
const READDRESSED_HOST: u8 = 200;

impl<S: SwarmSpec> Readdress<S> for Docker<S>
where
    S::Array<Arc<ManagedContainer>>: Sync,
{
    /// Within the network's subnet, to the address with the last octet `READDRESSED_HOST`.
    /// Docker isn't told: its inspect API, and with it the address [`Block`] and [`Isolate`] use,
    /// keeps the old one.
    async fn readdress(&self, runtime: S) {
        let container = &self.container_map[runtime];
        let old = container.container_ip(self.network.name()).await;
        let prefix = self.network.ipv4_prefix_len().await;
        assert!(
            prefix <= 24,
            "the last octet alone must stay within the /{prefix} subnet"
        );
        let (network_part, host) = old
            .rsplit_once('.')
            .unwrap_or_else(|| panic!("`{old}` is no IPv4 address"));
        assert_ne!(
            host,
            READDRESSED_HOST.to_string(),
            "`{runtime:?}` has the new address already"
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
        let output = container.exec(&["sh", "-c", &script]).await;
        assert!(
            output.success,
            "failed to move {runtime:?} from {old} to {new}: {}",
            output.stderr
        );
    }
}
