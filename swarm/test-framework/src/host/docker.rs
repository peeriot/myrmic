use std::{
    collections::HashSet,
    marker::PhantomData,
    net::IpAddr,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use super::{Id, Remote, Router, Shared};
use crate::{
    docker,
    myrmic::{self, Myrmic},
    overlay,
};

/// A docker container on a [`crate::host_set::Docker`], which creates it
/// ([`crate::host_set::Docker::new_host`]). The container has `myrmic` and `zenoh-router` on its
/// `PATH`, and keeps the data of its runtimes on a file system of its own: `S` says which one,
/// [`Tmpfs`] (the default) or [`Volume`]. Clones are the same host.
///
/// Its network faults ([`Self::isolate`], [`Self::block`], [`Self::degrade`], [`Self::readdress`])
/// are nftables rules, traffic control (`tc`) rules and address changes inside the container. So
/// they act on everything in the container except its loopback traffic: what runs on one host can
/// always reach itself.
pub struct Docker<S = Tmpfs> {
    host: Host,
    storage: PhantomData<fn() -> S>,
}

/// The storage of a [`Docker`] host whose runtimes keep their data on a small file system in the
/// container's memory, a tmpfs. [`Docker::fill_disk`] fills it.
pub enum Tmpfs {}

/// The storage of a [`Docker`] host whose runtimes keep their data on a docker volume of the
/// container's own, on the disk of the machine that runs docker
/// ([`crate::host_set::Docker::new_volume_host`]). It holds more than fits in memory, also when
/// the host's memory is limited ([`Docker::set_memory_limit`]), and its data doesn't count as the
/// container's memory. It can't be filled: docker can't limit its size.
pub enum Volume {}

/// The storage a [`Docker`] host's type names, so that the container's data directory is set up
/// from the type alone.
pub(crate) trait Storage {
    /// `container` with the data directory this storage mounts
    fn mount_data(container: docker::ContainerBuilder<'_>) -> docker::ContainerBuilder<'_>;
}

impl Storage for Tmpfs {
    fn mount_data(container: docker::ContainerBuilder<'_>) -> docker::ContainerBuilder<'_> {
        container.tmpfs(DOCKER_DATA_DIR, DOCKER_DATA_DIR_OPTIONS)
    }
}

impl Storage for Volume {
    fn mount_data(container: docker::ContainerBuilder<'_>) -> docker::ContainerBuilder<'_> {
        container.volume(DOCKER_DATA_DIR)
    }
}

/// The host behind a [`Docker`], whatever its storage. The guards of its faults ([`Block`],
/// [`Degraded`]) and of its traffic counts ([`TrafficCounter`]) hold it, as they work on hosts of
/// either kind.
#[derive(Clone)]
struct Host {
    container: Arc<docker::ManagedContainer>,
    /// the docker network it is on
    network: Arc<docker::ManagedNetwork>,
    shared: Arc<Shared>,
    peer_rules: Arc<Mutex<PeerRules>>,
    label: Arc<str>,
}

/// The rules in other hosts' containers that name this host's address, besides blocks (which its
/// firewall lists, see [`Shared`]). They keep acting on the address they name, so the host can't
/// move while there are any ([`Docker::readdress`]).
#[derive(Default)]
struct PeerRules {
    /// the hosts that degrade the traffic they send to this host ([`Docker::degrade`])
    degrading: HashSet<Id>,
    /// the hosts that count their traffic with this host ([`Docker::count_traffic`])
    counting: HashSet<Id>,
}

/// Where a runtime in a container keeps its data, i.e. its database and logs: the CLI's data
/// directory when it runs as root (`$HOME/.local/share/myrmic`).
const DOCKER_DATA_DIR: &str = "/root/.local/share/myrmic";

/// The mount options of the tmpfs at [`DOCKER_DATA_DIR`]. The tmpfs is a small file system of the
/// container's own, so [`Docker::fill_disk`] fills it instead of the machine's disk. 128 MB is far
/// more than a test's runtime writes (a few MB), and small enough to fill in RAM. A database that
/// stopped because the disk was full only reopens once 64 MB are free again, so the disk must be
/// larger than that.
const DOCKER_DATA_DIR_OPTIONS: &str = "size=128m";

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

/// A shell line that sets `dev` to the container's network interface whose address is `$1`.
const FIND_DEVICE: &str = r#"dev=$(ip -o -4 addr show | awk -v address="$1" 'index($4, address "/") == 1 { print $2; exit }')"#;

/// The rate of the `htb` classes [`Docker::degrade`] sorts a container's outgoing traffic into: far
/// more than a docker network carries, so that the classes only sort and never slow anything down.
/// netem slows a degraded link down to the rate the test asks for.
const UNLIMITED_RATE: &str = "100gbit";

/// How many packets a degraded link holds back at most. netem keeps every packet it delays in its
/// queue, and its default of 1000 packets would drop a fast link's traffic beyond 1000 packets per
/// delay, a loss the test didn't ask for.
const NETEM_LIMIT: &str = "100000";

impl<S> Docker<S> {
    /// The container `container` on the docker network `network`, at the address `address` there,
    /// with nothing in its firewall, whose caller mounted its data directory as `S` says
    /// ([`Storage::mount_data`]).
    /// Prepares the container's interface for degrading and counting its traffic:
    ///
    /// - It hands every packet to the rest of the kernel as it goes on the wire. By default it
    ///   bundles a TCP stream's packets into blocks of up to 64 KB (segmentation and receive
    ///   offloads), and the faults and counters would act on those blocks: losing a block loses
    ///   dozens of packets at once, and a block counts one set of headers for all of them.
    /// - It sorts its outgoing traffic into classes (an `htb` qdisc, class 1:1 for everything no
    ///   [`Self::degrade`] sorts elsewhere), set up once here so that concurrent degrades find
    ///   it ready.
    ///
    /// The container is removed once the last host or shim that uses it is dropped.
    pub(crate) async fn new(
        container: Arc<docker::ManagedContainer>,
        network: Arc<docker::ManagedNetwork>,
        address: IpAddr,
    ) -> Self {
        let label = Arc::from(format!("container {}", container.get_id()));
        let shared = Arc::new(Shared::default());
        shared
            .lock_addresses()
            .insert(network.get_name().to_owned(), address);
        let host = Self {
            host: Host {
                container,
                network,
                shared,
                peer_rules: Arc::default(),
                label,
            },
            storage: PhantomData,
        };
        // `$1`: the container's address
        let script = format!(
            "set -eu; {FIND_DEVICE}; \
             ethtool -K \"$dev\" tso off gso off gro off; \
             tc qdisc add dev \"$dev\" root handle 1: htb default 1; \
             tc class add dev \"$dev\" parent 1: classid 1:1 htb rate {UNLIMITED_RATE}"
        );
        let address = address.to_string();
        host.host
            .exec(
                "prepare the network interface",
                &["sh", "-c", &script, "sh", &address],
            )
            .await;
        host
    }

    /// its identity
    pub(crate) fn get_id(&self) -> Id {
        self.host.get_id()
    }

    /// The CLI in this container, attached by `attachment`. Unlike the other hosts' CLIs, it needs
    /// no state directories of its own: the container's are its own already. The CLIs of one
    /// container share them.
    pub fn myrmic(&self, attachment: impl Into<overlay::Attachment>) -> Myrmic<myrmic::Docker<S>> {
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

    /// Limit the memory of everything in this container to `bytes`, as on a machine with that much
    /// memory and no swap: once its processes use more and the kernel can't free any, the kernel
    /// kills one of them. The data on a [`Tmpfs`] counts as memory too; on a [`Volume`], only the
    /// kernel's cache of it does, which the kernel frees as needed. A later call replaces the
    /// limit.
    pub async fn set_memory_limit(&self, bytes: u64) {
        self.host.container.set_memory_limit(bytes).await;
    }

    /// Set this container's wall clock `ahead_secs` seconds ahead of the other containers' clocks
    /// (behind them if negative; 0 puts it back in step), as a clock step on a real machine would.
    /// The monotonic clock doesn't change, as with a real step. Returns once the processes in the
    /// container read the new time.
    pub async fn skew_clock(&self, ahead_secs: i64) {
        // `$1`: the offset, `$2`: the file
        let write = r#"printf '%s\n' "$1" > "$2""#;
        let offset = format!("{ahead_secs:+}");
        self.host
            .exec(
                "skew the clock",
                &["sh", "-c", write, "sh", &offset, FAKETIME_FILE],
            )
            .await;
        tokio::time::sleep(FAKETIME_PICKUP).await;
    }

    /// Cut this host off its network, as if its cable were pulled: drop all of its traffic except
    /// its loopback traffic. Everything on the host keeps running, and the other hosts only notice
    /// that it went silent.
    pub async fn isolate(&self) {
        self.host.exec("isolate", &["nft", ISOLATE_RULES]).await;
        self.host.shared.lock_firewall().isolated = true;
    }

    /// Lift [`Self::isolate`]. A [`Self::block`] stays.
    pub async fn heal(&self) {
        self.host
            .exec("heal", &["nft", "delete table inet isolate"])
            .await;
        self.host.shared.lock_firewall().isolated = false;
    }

    /// Drop all traffic between this host and `peer`, another host on the same network, until
    /// [`Block::unblock`]. Dropping the returned [`Block`] keeps the traffic dropped. Both hosts
    /// stay connected to everything else, and may still reach each other through a third host.
    ///
    /// Panics if the two hosts are blocked from each other already.
    pub async fn block<P>(&self, peer: &Docker<P>) -> Block {
        let (a, b) = (&self.host, &peer.host);
        assert!(
            !a.shared.lock_firewall().blocked.contains(&b.get_id()),
            "{} and {} are blocked from each other already",
            a.label,
            b.label
        );
        a.drop_traffic(b).await;
        b.drop_traffic(a).await;
        Block {
            a: a.clone(),
            b: b.clone(),
        }
    }

    /// Degrade the traffic this host sends to `peer`, another host on the same network, until
    /// [`Degraded::restore`]: as on a slow, unreliable link, every packet to `peer` waits `delay`,
    /// give or take up to `jitter`, `loss_percent` percent of them get lost, and with a `rate_bps`
    /// they leave at most at that many bits per second. Dropping the returned [`Degraded`] keeps
    /// the link degraded.
    ///
    /// It acts on one direction of the link: `peer`'s traffic to this host travels as before. So a
    /// request and its answer only both wait `delay` when the test degrades the link on both hosts
    /// ([`Self::degrade`] on `peer` too). Jitter reorders packets, as on a real link.
    ///
    /// Panics if this host degrades its traffic to `peer` already.
    pub async fn degrade<P>(
        &self,
        peer: &Docker<P>,
        delay: Duration,
        jitter: Duration,
        loss_percent: f64,
        rate_bps: Option<u64>,
    ) -> Degraded {
        let (host, peer) = (&self.host, &peer.host);
        assert!(
            (0.0..=100.0).contains(&loss_percent),
            "a loss of {loss_percent} % is no percentage"
        );
        let prefix = host.network.get_ipv4_prefix_len().await;
        assert!(
            prefix >= 16,
            "the last two octets name a host only within a /16 subnet, not a /{prefix} one"
        );
        let first = peer.lock_peer_rules().degrading.insert(host.get_id());
        assert!(
            first,
            "{} degrades its traffic to {} already",
            host.label, peer.label
        );

        // `$1`: this host's address, `$2`: the class of the traffic to `peer`, `$3`: the filter's
        // priority, `$4`: `peer`'s address, the rest: netem's parameters. The `htb` root is there
        // since the host was created (see `Self::new`).
        let script = format!(
            "set -eu; {FIND_DEVICE}; class=1:$2; priority=$3; peer=$4; shift 4; \
             tc class add dev \"$dev\" parent 1: classid \"$class\" htb rate {UNLIMITED_RATE}; \
             tc qdisc add dev \"$dev\" parent \"$class\" netem \"$@\"; \
             tc filter add dev \"$dev\" parent 1: protocol ip prio \"$priority\" u32 \
                match ip dst \"$peer/32\" flowid \"$class\""
        );
        let address = host.get_address().to_string();
        let class = get_link_class(host, peer);
        let class_id = format!("{class:x}");
        let priority = class.to_string();
        let peer_address = peer.get_address_seen_by(host).to_string();
        let delay = format!("{}us", delay.as_micros());
        let jitter = format!("{}us", jitter.as_micros());
        let loss = format!("{loss_percent}%");
        let rate = rate_bps.map(|rate| format!("{rate}bit"));
        let mut args = vec![
            "sh",
            "-c",
            &script,
            "sh",
            &address,
            &class_id,
            &priority,
            &peer_address,
            "limit",
            NETEM_LIMIT,
            "delay",
            &delay,
            &jitter,
            "loss",
            &loss,
        ];
        if let Some(rate) = &rate {
            args.extend(["rate", rate]);
        }
        host.exec(&format!("degrade the link to {}", peer.label), &args)
            .await;
        Degraded {
            host: host.clone(),
            peer: peer.clone(),
        }
    }

    /// Count the traffic between this host and `peer`, another host on the same network, from now
    /// on, until [`TrafficCounter::stop`]: the bytes of the IP packets this host's firewall lets
    /// through, headers included. A packet counts as sent before a degraded link
    /// ([`Self::degrade`]) gets it, so the packets that link loses count too. Dropping the returned
    /// [`TrafficCounter`] keeps counting.
    ///
    /// Panics if this host counts its traffic with `peer` already.
    pub async fn count_traffic<P>(&self, peer: &Docker<P>) -> TrafficCounter {
        let (host, peer) = (&self.host, &peer.host);
        let first = peer.lock_peer_rules().counting.insert(host.get_id());
        assert!(
            first,
            "{} counts its traffic with {} already",
            host.label, peer.label
        );

        let table = get_count_table(peer);
        let peer_address = peer.get_address_seen_by(host);
        // after the firewall's drops (priority 0), so that dropped packets don't count
        let rules = format!(
            "add table inet {table}; \
             add counter inet {table} sent; \
             add counter inet {table} received; \
             add chain inet {table} input {{ type filter hook input priority 10; policy accept; }}; \
             add chain inet {table} output {{ type filter hook output priority 10; policy accept; }}; \
             add rule inet {table} input ip saddr {peer_address} counter name received; \
             add rule inet {table} output ip daddr {peer_address} counter name sent"
        );
        host.exec(
            &format!("count the traffic with {}", peer.label),
            &["nft", &rules],
        )
        .await;
        TrafficCounter {
            host: host.clone(),
            peer: peer.clone(),
        }
    }

    /// Move this host to another address on its network, as when a machine's address changes.
    /// Neither what runs on the host nor the other hosts are told; finding the host again is their
    /// business. From then on, the network has the host at the new address: what starts afterwards
    /// connects to it there, while what already runs keeps the endpoints it started with. Docker
    /// isn't told either, and nothing needs it to be: docker is only asked for a host's address
    /// when the host is created.
    ///
    /// No rule may name the host's address when it moves: not a block of it ([`Self::block`]), and
    /// no other host's degraded link to it or counter of its traffic ([`Self::degrade`],
    /// [`Self::count_traffic`]). Those rules would keep acting on its old address.
    pub async fn readdress(&self) {
        let host = &self.host;
        // a block puts rules on both hosts, so the host's own firewall names every block of it
        assert!(
            host.shared.lock_firewall().blocked.is_empty(),
            "a blocked host can't move: its blocks name its old address"
        );
        {
            let peer_rules = host.lock_peer_rules();
            assert!(
                peer_rules.degrading.is_empty() && peer_rules.counting.is_empty(),
                "{} can't move: other hosts' degraded links or traffic counters name its old \
                 address",
                host.label
            );
        }
        let old = host.get_address();
        let prefix = host.network.get_ipv4_prefix_len().await;
        assert!(
            prefix <= 24,
            "the last octet alone must stay within the /{prefix} subnet"
        );
        let IpAddr::V4(old) = old else {
            panic!("`{old}` is no IPv4 address")
        };
        let [a, b, c, _] = old.octets();
        let new = std::net::Ipv4Addr::new(a, b, c, host.network.take_moved_host_octet());
        // `$1`: the old address. Dropping the old address also drops the on-link route the default
        // route needs, so that one is captured first and put back; its words are split again on
        // purpose, it is a whole route specification.
        let script = format!(
            "set -eu; {FIND_DEVICE}; \
             default_route=$(ip -4 route show default | head -n 1); \
             ip addr del {old}/{prefix} dev \"$dev\"; \
             ip addr add {new}/{prefix} dev \"$dev\"; \
             [ -z \"$default_route\" ] || ip -4 route replace $default_route"
        );
        let old_address = old.to_string();
        host.exec(
            &format!("move {old} to {new}"),
            &["sh", "-c", &script, "sh", &old_address],
        )
        .await;
        host.shared
            .lock_addresses()
            .insert(host.network.get_name().to_owned(), IpAddr::V4(new));
    }
}

impl Docker<Tmpfs> {
    /// Fill the disk on which the runtimes in this container keep their data until only `room`
    /// bytes are free, as when a machine's disk runs full. Everything keeps running, and writes
    /// start failing once the remaining room is used up.
    pub async fn fill_disk(&self, room: u64) {
        // `$1`: the data directory, `$2`: the filler, `$3`: the room
        let fill = r#"fallocate -l $(( $(df --output=avail -B1 "$1" | tail -n 1) - $3 )) "$2""#;
        let room = room.to_string();
        self.host
            .exec(
                "fill the disk",
                &["sh", "-c", fill, "sh", DOCKER_DATA_DIR, FILLER, &room],
            )
            .await;
    }

    /// free the disk [`Self::fill_disk`] filled
    pub async fn free_disk(&self) {
        self.host.exec("free the disk", &["rm", FILLER]).await;
    }
}

impl<S> Clone for Docker<S> {
    fn clone(&self) -> Self {
        Self {
            host: self.host.clone(),
            storage: PhantomData,
        }
    }
}

impl<S> Remote for Docker<S> {
    /// `docker exec <container> program args`
    fn command(&self, program: &str, args: &[&str]) -> std::process::Command {
        let container = &self.host.container;
        let mut command = container.docker_cli();
        command
            .args(["exec", container.get_id(), program])
            .args(args);
        command
    }
}

impl Host {
    /// its identity
    fn get_id(&self) -> Id {
        Id {
            shared: Arc::downgrade(&self.shared),
            label: self.label.clone(),
        }
    }

    /// the address it has now on its network
    fn get_address(&self) -> IpAddr {
        self.get_id().get_address(self.network.get_name())
    }

    /// the address it has now on the network that it shares with `other`
    fn get_address_seen_by(&self, other: &Host) -> IpAddr {
        self.get_id().get_address(other.network.get_name())
    }

    fn lock_peer_rules(&self) -> MutexGuard<'_, PeerRules> {
        self.peer_rules.lock().expect("not poisoned")
    }

    /// Run `cmd` in the container, which does what `what` says (for the panic message). Panics if
    /// it fails: the faults' commands only fail if the test infrastructure is broken.
    async fn exec(&self, what: &str, cmd: &[&str]) -> String {
        let output = self.container.exec(cmd).await;
        assert!(
            output.success,
            "failed to {what} on {}: {}",
            self.label, output.stderr
        );
        output.stdout
    }

    /// Drop all traffic between this host and `peer`, in a table of this host's own for `peer`
    /// ([`get_block_table`]), so that [`Self::pass_traffic`] can remove exactly these rules.
    async fn drop_traffic(&self, peer: &Host) {
        let table = get_block_table(peer);
        let peer_address = peer.get_address_seen_by(self);
        let rules = format!(
            "add table inet {table}; \
             add chain inet {table} input {{ type filter hook input priority 0; policy accept; }}; \
             add chain inet {table} output {{ type filter hook output priority 0; policy accept; }}; \
             add rule inet {table} input ip saddr {peer_address} drop; \
             add rule inet {table} output ip daddr {peer_address} drop"
        );
        self.exec(
            &format!("drop the traffic with {}", peer.label),
            &["nft", &rules],
        )
        .await;
        self.shared.lock_firewall().blocked.insert(peer.get_id());
    }

    /// Let the traffic between this host and `peer` through again: undo [`Self::drop_traffic`].
    async fn pass_traffic(&self, peer: &Host) {
        let table = get_block_table(peer);
        self.exec(
            &format!("pass the traffic with {}", peer.label),
            &["nft", &format!("delete table inet {table}")],
        )
        .await;
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
    a: Host,
    b: Host,
}

impl Block {
    /// Let the traffic between the two hosts through again. What runs on them may reconnect.
    pub async fn unblock(self) {
        self.a.pass_traffic(&self.b).await;
        self.b.pass_traffic(&self.a).await;
    }
}

/// A host's traffic to a peer, degraded by [`Docker::degrade`]. Dropping it keeps the traffic
/// degraded.
pub struct Degraded {
    host: Host,
    peer: Host,
}

impl Degraded {
    /// Let the host's traffic to the peer travel as before. Packets held back until now still
    /// arrive late.
    pub async fn restore(self) {
        let Self { host, peer } = self;
        // `$1`: this host's address, `$2`: the class of the traffic to the peer, `$3`: the
        // filter's priority
        let script = format!(
            "set -eu; {FIND_DEVICE}; \
             tc filter del dev \"$dev\" parent 1: prio \"$3\"; \
             tc qdisc del dev \"$dev\" parent \"1:$2\"; \
             tc class del dev \"$dev\" classid \"1:$2\""
        );
        let address = host.get_address().to_string();
        let class = get_link_class(&host, &peer);
        host.exec(
            &format!("restore the link to {}", peer.label),
            &[
                "sh",
                "-c",
                &script,
                "sh",
                &address,
                &format!("{class:x}"),
                &class.to_string(),
            ],
        )
        .await;
        let was_degrading = peer.lock_peer_rules().degrading.remove(&host.get_id());
        assert!(
            was_degrading,
            "{} didn't degrade its traffic to {}",
            host.label, peer.label
        );
    }
}

/// The traffic between a host and a peer, counted by [`Docker::count_traffic`]. Dropping it keeps
/// counting.
pub struct TrafficCounter {
    host: Host,
    peer: Host,
}

impl TrafficCounter {
    /// how many bytes the host sent to the peer since the count started
    pub async fn get_sent_bytes(&self) -> u64 {
        self.get_bytes("sent").await
    }

    /// how many bytes the host received from the peer since the count started
    pub async fn get_received_bytes(&self) -> u64 {
        self.get_bytes("received").await
    }

    /// Stop counting.
    pub async fn stop(self) {
        let Self { host, peer } = self;
        let table = get_count_table(&peer);
        host.exec(
            &format!("stop counting the traffic with {}", peer.label),
            &["nft", &format!("delete table inet {table}")],
        )
        .await;
        let was_counting = peer.lock_peer_rules().counting.remove(&host.get_id());
        assert!(
            was_counting,
            "{} didn't count its traffic with {}",
            host.label, peer.label
        );
    }

    /// the bytes the nftables counter `counter` of the count counted
    async fn get_bytes(&self, counter: &str) -> u64 {
        let table = get_count_table(&self.peer);
        let listing = self
            .host
            .exec(
                &format!("read the traffic with {}", self.peer.label),
                &["nft", "-j", "list", "counter", "inet", &table, counter],
            )
            .await;
        parse_counter_bytes(&listing)
    }
}

/// the bytes in `nft -j list counter`'s `listing` of one counter
fn parse_counter_bytes(listing: &str) -> u64 {
    let listing: serde_json::Value = serde_json::from_str(listing).expect("`nft -j` prints JSON");
    listing["nftables"]
        .as_array()
        .expect("`nft -j` lists its objects under `nftables`")
        .iter()
        .find_map(|object| object["counter"]["bytes"].as_u64())
        .unwrap_or_else(|| panic!("`nft -j list counter` lists no counter: {listing}"))
}

/// The number of the `htb` class, and of the filter that sorts into it, of the traffic `host`
/// sends to `peer` ([`Docker::degrade`]): the last two octets of `peer`'s address. They are unique
/// among the hosts of a network of /16 or smaller, which `degrade` checks. And they stay the same
/// while the link is degraded: `peer` can't move then ([`Docker::readdress`]).
fn get_link_class(host: &Host, peer: &Host) -> u16 {
    let IpAddr::V4(address) = peer.get_address_seen_by(host) else {
        panic!("{} has no IPv4 address", peer.label)
    };
    let [_, _, c, d] = address.octets();
    let class = u16::from_be_bytes([c, d]);
    // 0 is no class, and class 1 takes the traffic no filter sorts elsewhere; neither is a
    // container's address (`.0.0` is the network's own, `.0.1` its gateway's)
    assert!(class > 1, "{} has address {address}", peer.label);
    class
}

/// the name of the nftables table in which a host drops the traffic of `peer`
fn get_block_table(peer: &Host) -> String {
    format!("block_{}", peer.container.get_id())
}

/// the name of the nftables table in which a host counts its traffic with `peer`
fn get_count_table(peer: &Host) -> String {
    format!("count_{}", peer.container.get_id())
}
