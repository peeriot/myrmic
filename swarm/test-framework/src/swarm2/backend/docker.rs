use std::path::Path;
use std::sync::Arc;

use enum_map::EnumMap;
use sha2::{Digest, Sha256};
use tokio::sync::OnceCell;

use crate::{
    docker::{DockerDaemon, container::ManagedContainer, image::Image, network::ManagedNetwork},
    myrmic::{self, Myrmic},
    resolve_binary,
    swarm2::{SwarmBackend, SwarmSpec, join_enum_map},
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
    /// the router's endpoint, e.g. `tcp/172.23.0.5:7447`
    router_endpoint: String,
    /// the CLI on the test's host, connected to the router
    client: Myrmic<myrmic::Local>,
}

/// The image's Dockerfile; its binaries come from the workspace build.
const DOCKERFILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/docker/Dockerfile");

/// The environment variable naming the container the test runs in, if it runs in one.
pub const TEST_CONTAINER: &str = "MYRMIC_TEST_CONTAINER";

/// The port `zenoh-router` listens on: zenoh's default for routers.
const ROUTER_PORT: u16 = 7447;

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
                let container = ManagedContainer::run(
                    image,
                    dockerd.clone(),
                    &["sleep", "infinity"],
                    Some(network),
                    &name,
                )
                .await;
                Arc::new(container)
            }
        })
        .await;
        let router_name = format!("{}-router", network.name());
        let router = ManagedContainer::run(
            image,
            dockerd.clone(),
            &["zenoh-router"],
            Some(network.clone()),
            &router_name,
        )
        .await;
        let router_endpoint = format!(
            "tcp/{}:{ROUTER_PORT}",
            router.container_ip(network.name()).await
        );

        Self {
            container_map,
            _router: router,
            client: Myrmic::local_connected(router_endpoint.clone()),
            router_endpoint,
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
        let mut config = zenoh::Config::default();
        config
            .set_mode(Some(zenoh::config::WhatAmI::Client))
            .expect("setting zenoh mode cannot fail");
        config
            .insert_json5(
                "connect/endpoints",
                &serde_json::json!([self.router_endpoint]).to_string(),
            )
            .expect("the router's address is a valid zenoh endpoint");
        config
            .insert_json5("scouting/multicast/enabled", "false")
            .expect("disabling multicast scouting cannot fail");
        zenoh::open(config)
            .await
            .expect("failed to connect to the swarm's router")
    }
}
