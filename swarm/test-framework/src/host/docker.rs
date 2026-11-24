use std::{net::IpAddr, sync::Arc, time::Duration};

use super::{Id, Remote, Router, Shared};
use crate::{
    docker,
    myrmic::{self, Myrmic},
    overlay,
};

/// A docker container on a [`crate::host_set::Docker`], which creates it
/// ([`crate::host_set::Docker::new_host`]). The container has `myrmic` and `zenoh-router` on its
/// `PATH`, and keeps the data of its runtimes on a small file system of its own. Clones are the
/// same host.
///
/// Its network faults ([`Self::isolate`], [`Self::block`], [`Self::readdress`]) are nftables rules
/// and address changes inside the container. So they act on everything in the container except its
/// loopback traffic: what runs on one host can always reach itself.
#[derive(Clone)]
pub struct Docker {
    container: Arc<docker::ManagedContainer>,
    /// the docker network it is on
    network: Arc<docker::ManagedNetwork>,
    shared: Arc<Shared>,
    label: Arc<str>,
}

/// Where a runtime in a container keeps its data, i.e. its database and logs: the CLI's data
/// directory when it runs as root (`$HOME/.local/share/myrmic`).
pub(crate) const DOCKER_DATA_DIR: &str = "/root/.local/share/myrmic";

/// The mount options of the tmpfs at [`DOCKER_DATA_DIR`]. The tmpfs is a small file system of the
/// container's own, so [`Docker::fill_disk`] fills it instead of the machine's disk. 128 MB is far
/// more than a test's runtime writes (a few MB), and small enough to fill in RAM. A database that
/// stopped because the disk was full only reopens once 64 MB are free again, so the disk must be
/// larger than that.
pub(crate) const DOCKER_DATA_DIR_OPTIONS: &str = "size=128m";

/// the file [`Docker::fill_disk`] fills [`DOCKER_DATA_DIR`] with
const FILLER: &str = "/root/.local/share/myrmic/filler";

/// The file holding a container's clock offset in seconds, which libfaketime reads (see the
/// Dockerfile).
const FAKETIME_FILE: &str = "/etc/faketime";

/// How long a runtime may take to read a new [`FAKETIME_FILE`]. libfaketime reads the file again at
/// most a second (its `FAKETIME_CACHE_DURATION`) after it last read it, and processes that read
/// their clock just before the change need one more such second.
const FAKETIME_PICKUP: Duration = Duration::from_secs(2);

/// The nftables commands of [`Docker::isolate`]: they drop all traffic into and out of the
/// container except its loopback traffic. The rules are in a table of their own, so
/// [`Docker::heal`] removes exactly these rules.
const ISOLATE_RULES: &str = "add table inet isolate; \
    add chain inet isolate input { type filter hook input priority 0; policy drop; }; \
    add chain inet isolate output { type filter hook output priority 0; policy drop; }; \
    add rule inet isolate input iif lo accept; \
    add rule inet isolate output oif lo accept";

impl Docker {
    /// The container `container` on the docker network `network`, at the address `address` there,
    /// with nothing in its firewall. The container is removed once the last host or shim that uses
    /// it is dropped.
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

    /// The CLI in this container, attached by `attachment`. Unlike the other hosts' CLIs, it needs
    /// no state directories of its own: the container's are its own already. The CLIs of one
    /// container share them.
    pub fn myrmic(&self, attachment: impl Into<overlay::Attachment>) -> Myrmic<myrmic::Docker> {
        Myrmic::new(
            myrmic::Docker::new(self.clone()),
            "myrmic".to_owned(),
            attachment.into(),
        )
    }

    /// A zenoh router in this container, as a new member of an overlay, attached by `attachment`.
    /// Returns once the router listens, so a swarm can attach through it right away (see
    /// [`Router`]).
    pub async fn router(&self, attachment: impl Into<overlay::Attachment>) -> Router<Self> {
        let member = attachment.into().add_member(self.get_id());
        Router::start(self.clone(), "zenoh-router", member).await
    }

    /// Fill the disk on which the runtimes in this container keep their data until only `room`
    /// bytes are free, as when a machine's disk runs full. Everything keeps running, and writes
    /// start failing once the remaining room is used up.
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

    /// Set this container's wall clock `ahead_secs` seconds ahead of the other containers' clocks
    /// (behind them if negative; 0 puts it back in step), as a clock step on a real machine would.
    /// The monotonic clock doesn't change, as with a real step. Returns once the processes in the
    /// container read the new time.
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

    /// Cut this host off its network, as if its cable were pulled: drop all of its traffic except
    /// its loopback traffic. Everything on the host keeps running, and the other hosts only notice
    /// that it went silent.
    pub async fn isolate(&self) {
        let output = self.container.exec(&["nft", ISOLATE_RULES]).await;
        assert!(
            output.success,
            "failed to isolate {}: {}",
            self.label, output.stderr
        );
        self.shared.lock_firewall().isolated = true;
    }

    /// Lift [`Self::isolate`]. A [`Self::block`] stays.
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

    /// Drop all traffic between this host and `peer`, another host on the same network, until
    /// [`Block::unblock`]. Dropping the returned [`Block`] keeps the traffic dropped. Both hosts
    /// stay connected to everything else, and may still reach each other through a third host.
    ///
    /// Panics if the two hosts are blocked from each other already.
    pub async fn block(&self, peer: &Docker) -> Block {
        assert!(
            !self.shared.lock_firewall().blocked.contains(&peer.get_id()),
            "{} and {} are blocked from each other already",
            self.label,
            peer.label
        );
        self.drop_traffic(peer).await;
        peer.drop_traffic(self).await;
        Block {
            a: self.clone(),
            b: peer.clone(),
        }
    }

    /// Move this host to another address on its network, as when a machine's address changes.
    /// Neither what runs on the host nor the other hosts are told; finding the host again is their
    /// business. From then on, the network has the host at the new address: what starts afterwards
    /// connects to it there, while what already runs keeps the endpoints it started with. Docker
    /// isn't told either, and nothing needs it to be: docker is only asked for a host's address
    /// when the host is created.
    ///
    /// The host must not be blocked from another host ([`Self::block`]) when it moves: those rules
    /// drop the traffic of its old address.
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
        let [a, b, c, _] = old.octets();
        let new = std::net::Ipv4Addr::new(a, b, c, self.network.take_moved_host_octet());
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

    /// Drop all traffic between this host and `peer`, in a table of this host's own for `peer`
    /// ([`get_block_table`]), so that [`Self::pass_traffic`] can remove exactly these rules.
    async fn drop_traffic(&self, peer: &Docker) {
        let table = get_block_table(peer);
        let peer_address = peer.get_id().get_address(self.network.name());
        let rules = format!(
            "add table inet {table}; \
             add chain inet {table} input {{ type filter hook input priority 0; policy accept; }}; \
             add chain inet {table} output {{ type filter hook output priority 0; policy accept; }}; \
             add rule inet {table} input ip saddr {peer_address} drop; \
             add rule inet {table} output ip daddr {peer_address} drop"
        );
        let output = self.container.exec(&["nft", &rules]).await;
        assert!(
            output.success,
            "failed to drop the traffic between {} and {}: {}",
            self.label, peer.label, output.stderr
        );
        self.shared.lock_firewall().blocked.insert(peer.get_id());
    }

    /// Let the traffic between this host and `peer` through again: undo [`Self::drop_traffic`].
    async fn pass_traffic(&self, peer: &Docker) {
        let table = get_block_table(peer);
        let output = self
            .container
            .exec(&["nft", &format!("delete table inet {table}")])
            .await;
        assert!(
            output.success,
            "failed to pass the traffic between {} and {}: {}",
            self.label, peer.label, output.stderr
        );
        let was_blocked = self.shared.lock_firewall().blocked.remove(&peer.get_id());
        assert!(
            was_blocked,
            "{} was not blocked from {}",
            self.label, peer.label
        );
    }
}

/// Two hosts blocked from each other by [`Docker::block`]. Dropping it keeps them blocked.
pub struct Block {
    a: Docker,
    b: Docker,
}

impl Block {
    /// Let the traffic between the two hosts through again. What runs on them may reconnect.
    pub async fn unblock(self) {
        self.a.pass_traffic(&self.b).await;
        self.b.pass_traffic(&self.a).await;
    }
}

/// the name of the nftables table in which a host drops the traffic of `peer`
fn get_block_table(peer: &Docker) -> String {
    format!("block_{}", peer.container.id())
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
