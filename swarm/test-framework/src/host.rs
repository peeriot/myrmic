//! The machines a test's runtimes run on: the test's own machine ([`Local`]), a docker container
//! ([`Docker`]). A host hands out the CLI shims that run on it ([`crate::myrmic::Myrmic`]), and
//! through them the runtimes.
//!
//! # Using it
//!
//! ```ignore
//! use test_framework::{docker::DockerDaemon, host, network, overlay::Overlay};
//!
//! let group = Overlay::new(&network::Local).new_multicast_group();
//! let myrmic = host::Local.myrmic_isolated(&group);  // the CLI on the test's machine
//!
//! let network = network::Docker::new(&DockerDaemon::connect()).await;
//! let group = Overlay::new(&network).new_multicast_group();
//! let container = network.new_host().await;
//! let remote = container.myrmic(&group);  // the CLI in a container
//! ```
//!
//! # Design notes
//!
//! A host is not the same as a shim's backend ([`crate::myrmic::MyrmicBackend`]): the backend
//! only knows how to run a command there, while a host is the machine: its identity on its
//! network ([`network::HostId`]), the shims it carries (one per attachment or isolation) and, in
//! time, its faults. Its address is its network's to know (see [`crate::network`]), so it can
//! change.

use std::{sync::Arc, time::Duration};

use crate::{
    docker::{self, container::ManagedContainer},
    myrmic::{self, Myrmic},
    network, overlay,
};

/// The test's own machine.
pub struct Local;

impl Local {
    /// The CLI on this machine, attached by `attachment`, running the locally built myrmic binary
    /// (see [`crate::resolve_binary!`]). It has state directories of its own (runtime identities,
    /// databases, pid files, logs), removed with the last clone of the shim: a runtime's identity
    /// is its name within them, so tests that give their runtimes the same names can run in
    /// parallel, and nothing piles up in the machine's own directories.
    pub fn myrmic_isolated(
        &self,
        attachment: impl Into<overlay::Attachment>,
    ) -> Myrmic<myrmic::Local> {
        Myrmic::new(myrmic::Local::isolated(), local_binary(), attachment.into())
    }

    /// Like [`Self::myrmic_isolated`], but on a group of its own, in an overlay of its own on
    /// [`network::Local`]: the CLI for a test whose runtimes all run on this machine and that never
    /// needs the group itself.
    pub fn myrmic_isolated_alone(&self) -> Myrmic<myrmic::Local> {
        let group = overlay::Overlay::new(&network::Local).new_multicast_group();
        self.myrmic_isolated(&group)
    }
}

/// A docker container on a [`crate::network::Docker`], which creates it
/// ([`crate::network::Docker::new_host`]). It has `myrmic` and `zenoh-router` on its `PATH`, and
/// keeps the data of its runtimes on a small file system of its own. Clones are the same host.
#[derive(Clone)]
pub struct Docker {
    container: Arc<ManagedContainer>,
    id: network::HostId,
}

/// Where a runtime keeps its data in its container: the CLI's data directory as root
/// (`$HOME/.local/share/myrmic`), with the runtime's database and logs.
pub(crate) const DOCKER_DATA_DIR: &str = "/root/.local/share/myrmic";

/// The mount options of the tmpfs at [`DOCKER_DATA_DIR`]. Its own small file system, so
/// [`Docker::fill_disk`] fills a disk of the container's own instead of the machine's. 128 MB is
/// far more than a test's runtime writes (a few MB), and little enough to fill in RAM. A database
/// that a full disk stopped reopens once 64 MB are free again, so the disk must be larger than
/// that.
pub(crate) const DOCKER_DATA_DIR_OPTIONS: &str = "size=128m";

/// the file [`Docker::fill_disk`] fills [`DOCKER_DATA_DIR`] with
const FILLER: &str = "/root/.local/share/myrmic/filler";

/// The file holding a container's clock offset in seconds, which libfaketime reads (see the
/// Dockerfile).
const FAKETIME_FILE: &str = "/etc/faketime";

/// How long a runtime may take to read a new [`FAKETIME_FILE`]: libfaketime reads it again at most
/// a second (its `FAKETIME_CACHE_DURATION`) after it last did, and once more for the processes
/// that read their clock just before the change.
const FAKETIME_PICKUP: Duration = Duration::from_secs(2);

impl Docker {
    /// The container `container` with the id `id` on its docker network; it is removed once the
    /// last host or shim using it drops.
    pub(crate) fn new(container: Arc<ManagedContainer>, id: network::HostId) -> Self {
        Self { container, id }
    }

    /// its identity on its docker network, by which the network knows its address and cuts
    pub(crate) fn get_id(&self) -> network::HostId {
        self.id
    }

    /// the container, for the faults its network puts on it
    pub(crate) fn get_container(&self) -> &ManagedContainer {
        &self.container
    }

    /// the CLI in this container, attached by `attachment`
    pub fn myrmic(&self, attachment: impl Into<overlay::Attachment>) -> Myrmic<myrmic::Docker> {
        Myrmic::new(
            myrmic::Docker::new(self.clone()),
            "myrmic".to_owned(),
            attachment.into(),
        )
    }

    /// A zenoh router in this container, a new member of an overlay attached by `attachment`.
    /// Returns once it listens, so a swarm can attach through it right away (see
    /// [`docker::Router`]).
    pub async fn router(&self, attachment: impl Into<overlay::Attachment>) -> docker::Router {
        let member = attachment.into().add_member(self.id);
        docker::Router::start(self.clone(), member).await
    }

    /// Fill the disk the runtimes in this container keep their data on up to `room` free bytes,
    /// as when a machine's disk runs full. What runs keeps running, and its writes start failing
    /// once the room is used up.
    pub async fn fill_disk(&self, room: u64) {
        // `$1`: the data directory, `$2`: the filler, `$3`: the room
        let fill = r#"fallocate -l $(( $(df --output=avail -B1 "$1" | tail -n 1) - $3 )) "$2""#;
        let room = room.to_string();
        let output = self
            .container
            .exec(&["sh", "-c", fill, "sh", DOCKER_DATA_DIR, FILLER, &room])
            .await;
        assert!(
            output.success,
            "failed to fill the disk of container {}: {}",
            self.container.id(),
            output.stderr
        );
    }

    /// free the disk [`Self::fill_disk`] filled
    pub async fn free_disk(&self) {
        let output = self.container.exec(&["rm", FILLER]).await;
        assert!(
            output.success,
            "failed to free the disk of container {}: {}",
            self.container.id(),
            output.stderr
        );
    }

    /// Set this container's wall clock to `ahead_secs` seconds ahead of the others' (behind them
    /// if negative; 0 puts it back in step), as a clock step on a machine would. Its monotonic
    /// clock stays as it is, like with a real step. Returns once what runs here reads it.
    pub async fn skew_clock(&self, ahead_secs: i64) {
        // `$1`: the offset, `$2`: the file
        let write = r#"printf '%s\n' "$1" > "$2""#;
        let offset = format!("{ahead_secs:+}");
        let output = self
            .container
            .exec(&["sh", "-c", write, "sh", &offset, FAKETIME_FILE])
            .await;
        assert!(
            output.success,
            "failed to skew the clock of container {}: {}",
            self.container.id(),
            output.stderr
        );
        tokio::time::sleep(FAKETIME_PICKUP).await;
    }
}

/// the locally built myrmic binary (see [`crate::resolve_binary!`])
fn local_binary() -> String {
    let binary = crate::resolve_binary!("myrmic");
    binary
        .to_str()
        .unwrap_or_else(|| panic!("myrmic binary path {} is not UTF-8", binary.display()))
        .to_owned()
}
