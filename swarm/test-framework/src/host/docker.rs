use std::{net::IpAddr, sync::Arc, time::Duration};

use super::{Id, Remote, Router, Shared};
use crate::{
    docker,
    myrmic::{self, Myrmic},
    overlay,
};

/// A docker container on a [`crate::host_set::Docker`], which creates it
/// ([`crate::host_set::Docker::new_host`]). It has `myrmic` and `zenoh-router` on its `PATH`, and
/// keeps the data of its runtimes on a small file system of its own. Clones are the same host.
///
/// Its network faults ([`Self::isolate`], [`Self::block`], [`Self::readdress`]) are nftables
/// rules and address changes in its container, so they act on everything in the container but
/// its loopback traffic: what runs on one host always reaches itself.
#[derive(Clone)]
pub struct Docker {
    container: Arc<docker::ManagedContainer>,
    /// the docker network it is on
    network: Arc<docker::ManagedNetwork>,
    shared: Arc<Shared>,
    label: Arc<str>,
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
    /// The container `container` on the docker network `network`, at `address` there, with
    /// nothing in its firewall; it is removed once the last host or shim using it drops.
    pub(crate) fn new(
        container: Arc<docker::ManagedContainer>,
        network: Arc<docker::ManagedNetwork>,
        address: IpAddr,
    ) -> Self {
        let label = Arc::from(format!("container {}", container.id()));
        let shared = Arc::new(Shared::default());
        shared
            .lock_addresses()
            .insert(network.name().to_owned(), address);
        Self {
            container,
            network,
            shared,
            label,
        }
    }

    /// its identity
    pub(crate) fn get_id(&self) -> Id {
        Id {
            shared: Arc::downgrade(&self.shared),
            label: self.label.clone(),
        }
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
    /// Returns once it listens, so a swarm can attach through it right away (see [`Router`]).
    pub async fn router(&self, attachment: impl Into<overlay::Attachment>) -> Router<Self> {
        let member = attachment.into().add_member(self.get_id());
        Router::start(self.clone(), "zenoh-router", member).await
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

    /// Cut this host off its network: drop all its traffic but its loopback, as a host whose
    /// cable is pulled. What runs on it keeps running, and the others only notice its silence.
    pub async fn isolate(&self) {
        let output = self.container.exec(&["nft", ISOLATE_RULES]).await;
        assert!(
            output.success,
            "failed to isolate {}: {}",
            self.label, output.stderr
        );
        self.shared.lock_firewall().isolated = true;
    }

    /// lift [`Self::isolate`]
    pub async fn heal(&self) {
        let output = self
            .container
            .exec(&["nft", "delete table inet isolate"])
            .await;
        assert!(
            output.success,
            "failed to heal {}: {}",
            self.label, output.stderr
        );
        self.shared.lock_firewall().isolated = false;
    }

    /// Drop all traffic between this host and `peer`, a host on the same network. Both stay
    /// connected to everything else, and may still reach each other through it.
    pub async fn block(&self, peer: &Docker) {
        self.drop_traffic(peer).await;
        peer.drop_traffic(self).await;
    }

    /// Move this host to another address on its network, as when a host's address changes,
    /// without telling what runs on it or the others: finding back is their business. From then
    /// on the network has it at the new address, so what starts afterwards connects to it there,
    /// while what runs keeps the endpoints it started with. Docker isn't told, which nothing
    /// needs: docker is asked for a host's address only when the host is created.
    ///
    /// It must not be blocked from another host ([`Self::block`]): those rules drop the traffic
    /// of its old address.
    pub async fn readdress(&self) {
        // a block puts rules on both hosts, so the host's own firewall names every block of it
        assert!(
            self.shared.lock_firewall().blocked.is_empty(),
            "a blocked host can't move: its blocks name its old address"
        );
        let old = self.get_id().get_address(self.network.name());
        let prefix = self.network.ipv4_prefix_len().await;
        assert!(
            prefix <= 24,
            "the last octet alone must stay within the /{prefix} subnet"
        );
        let IpAddr::V4(old) = old else {
            panic!("`{old}` is no IPv4 address")
        };
        let [a, b, c, last] = old.octets();
        assert_ne!(last, READDRESSED_HOST, "`{old}` was moved already");
        let new = std::net::Ipv4Addr::new(a, b, c, READDRESSED_HOST);
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
        let output = self.container.exec(&["sh", "-c", &script]).await;
        assert!(
            output.success,
            "failed to move {old} to {new}: {}",
            output.stderr
        );
        self.shared
            .lock_addresses()
            .insert(self.network.name().to_owned(), IpAddr::V4(new));
    }

    /// Drop all traffic between this host and `peer`, in this host's [`BLOCK_TABLE`].
    async fn drop_traffic(&self, peer: &Docker) {
        let peer_address = peer.get_id().get_address(self.network.name());
        let rules = format!(
            "{BLOCK_TABLE}; \
             add rule inet block input ip saddr {peer_address} drop; \
             add rule inet block output ip daddr {peer_address} drop"
        );
        let output = self.container.exec(&["nft", &rules]).await;
        assert!(
            output.success,
            "failed to drop the traffic between {} and {}: {}",
            self.label, peer.label, output.stderr
        );
        self.shared.lock_firewall().blocked.insert(peer.get_id());
    }
}

impl Remote for Docker {
    /// `docker exec <container> program args`
    fn command(&self, program: &str, args: &[&str]) -> std::process::Command {
        let mut command = self.container.docker_cli();
        command
            .args(["exec", self.container.id(), program])
            .args(args);
        command
    }
}
