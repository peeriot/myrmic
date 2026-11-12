//! The machines a test's runtimes run on: the test's own machine ([`Local`]), a remote machine
//! over SSH ([`Ssh`]), a docker container ([`Docker`]). A host hands out the CLI shims that run
//! on it ([`crate::myrmic::Myrmic`]), and through them the runtimes.
//!
//! # Using it
//!
//! ```ignore
//! use test_framework::{host, network, overlay::Overlay};
//!
//! let group = Overlay::new(&network::Local).new_multicast_group();
//! let myrmic = host::Local.myrmic_isolated(&group);  // the CLI on the test's machine
//! let pi = host::Ssh::new("pi@rack-1", "192.168.1.21".parse()?, "myrmic");
//! let remote = pi.myrmic(&group);  // the CLI on the pi
//! ```
//!
//! # Design notes
//!
//! A host is not the same as a shim's backend ([`crate::myrmic::MyrmicBackend`]): the backend
//! only knows how to run a command there, while a host is the machine: its address, the shims it
//! carries (one per attachment or isolation) and, in time, its faults.

use std::{net::IpAddr, sync::Arc, time::Duration};

use crate::{
    docker::{self, container::ManagedContainer},
    myrmic::{self, Myrmic},
    overlay,
};

/// The test's own machine.
pub struct Local;

impl Local {
    /// The CLI on this machine, attached by `attachment`, running the locally built myrmic binary (see
    /// [`crate::resolve_binary!`]). It has state directories of its own (runtime identities,
    /// databases, pid files, logs), removed with the last clone of the shim: a runtime's identity
    /// is its name within them, so tests that give their runtimes the same names can run in
    /// parallel, and nothing piles up in the machine's own directories.
    pub fn myrmic_isolated(
        &self,
        attachment: impl Into<overlay::Attachment>,
    ) -> Myrmic<myrmic::Local> {
        Myrmic::new(
            myrmic::Local::isolated(),
            local_binary(),
            attachment.into(),
            overlay::LOCALHOST,
        )
    }
}

/// A remote machine, reached over SSH.
pub struct Ssh {
    backend: myrmic::Ssh,
    /// its address on the network the test's runtimes use, where a connect to one of its
    /// runtimes goes
    address: IpAddr,
    /// the myrmic binary on the machine, a path or a name on the remote user's `PATH`
    binary: String,
}

impl Ssh {
    /// The machine `login` (anything `ssh` accepts, e.g. `user@rack-node-1.example`) at `address`
    /// on the test's network, whose myrmic binary is `binary`: a path (e.g. where a benchmark
    /// harness `scp`'d it) or `myrmic` for the one on the remote user's `PATH`.
    pub fn new(login: impl Into<String>, address: IpAddr, binary: impl Into<String>) -> Self {
        Self {
            backend: myrmic::Ssh::new(login),
            address,
            binary: binary.into(),
        }
    }

    /// the CLI on this machine, on its own state directories, attached by `attachment`
    pub fn myrmic(&self, attachment: impl Into<overlay::Attachment>) -> Myrmic<myrmic::Ssh> {
        Myrmic::new(
            self.backend.clone(),
            self.binary.clone(),
            attachment.into(),
            self.address,
        )
    }
}

/// A docker container on a [`crate::network::Docker`], which creates it
/// ([`crate::network::Docker::new_host`]). It has `myrmic` and `zenoh-router` on its `PATH`, and
/// keeps the data of its runtimes on a small file system of its own.
pub struct Docker {
    container: Arc<ManagedContainer>,
    backend: myrmic::Docker,
    /// its address on its docker network
    address: IpAddr,
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
    /// The container `container` at `address` on its docker network; it is removed once the last
    /// host or shim using it drops.
    pub(crate) fn new(container: Arc<ManagedContainer>, address: IpAddr) -> Self {
        Self {
            backend: myrmic::Docker::attach(container.clone()),
            container,
            address,
        }
    }

    /// its address on its docker network
    pub fn get_address(&self) -> IpAddr {
        self.address
    }

    /// the container, for the faults its network puts on it
    pub(crate) fn get_container(&self) -> &ManagedContainer {
        &self.container
    }

    /// the CLI in this container, attached by `attachment`
    pub fn myrmic(&self, attachment: impl Into<overlay::Attachment>) -> Myrmic<myrmic::Docker> {
        Myrmic::new(
            self.backend.clone(),
            "myrmic".to_owned(),
            attachment.into(),
            self.address,
        )
    }

    /// A zenoh router in this container, a new member of an overlay attached by `attachment`.
    /// Returns once it listens, so a swarm can attach through it right away (see
    /// [`docker::Router`]).
    pub async fn router(&self, attachment: impl Into<overlay::Attachment>) -> docker::Router {
        let member = attachment.into().add_member(self.address);
        docker::Router::start(self.container.clone(), member).await
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
            "failed to fill the disk of {}: {}",
            self.address, output.stderr
        );
    }

    /// free the disk [`Self::fill_disk`] filled
    pub async fn free_disk(&self) {
        let output = self.container.exec(&["rm", FILLER]).await;
        assert!(
            output.success,
            "failed to free the disk of {}: {}",
            self.address, output.stderr
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
            "failed to skew the clock of {}: {}",
            self.address, output.stderr
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
