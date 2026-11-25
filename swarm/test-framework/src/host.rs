//! The machines a test's runtimes run on: the test's own machine ([`Local`]), a docker container
//! ([`Docker`]), or a machine the test reaches over SSH ([`Ssh`]). A test gets the `myrmic` CLI
//! of a host from the host ([`crate::myrmic::Myrmic`]), and starts runtimes through that CLI. A
//! docker or SSH host can also run zenoh routers ([`Router`]). For an SSH machine, [`ssh`] has
//! functions that wait until the machine is back after a reboot.
//!
//! # Using it
//!
//! ```ignore
//! use test_framework::{World, docker, host, host_set, overlay::Overlay, world};
//!
//! let group = Overlay::new(&World::local()).new_multicast_group();
//! let myrmic = host::Local.myrmic_isolated(&group); // the CLI on the test's machine
//!
//! let host_set = host_set::Docker::new(&docker::Daemon::connect()).await;
//! let group = Overlay::new(&world![host_set]).new_multicast_group();
//! let container = host_set.new_host().await;
//! let remote = container.myrmic(&group); // the CLI in a container
//! ```
//!
//! # Design notes
//!
//! A host is more than a CLI backend ([`crate::myrmic::MyrmicBackend`]). A backend only knows how
//! to run a command on the machine. A host is the machine itself: it has an identity ([`Id`]),
//! the CLIs that run on it (one per attachment or isolation), its faults, and an address on each
//! network it is on. A fault can change that address ([`Docker::readdress`]).
//!
//! A test works with a host's handle ([`Local`], [`Docker`], [`Ssh`]), and the handle owns the
//! host. Everything else that refers to the host (an overlay's members, a host set) holds its
//! [`Id`] instead. An id doesn't keep the host alive. Through it, others read what they need to
//! know about the host: its firewall, the faults that drop its traffic, and its addresses. A test
//! never deals with addresses: a host set reads them to find out how another host dials this one.

use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    net::IpAddr,
    sync::{Arc, Mutex, MutexGuard, Weak},
};

pub(crate) use self::{
    docker::Storage,
    local::{get_local_id, remove_local_address, set_local_address},
};
pub use self::{
    docker::{Block, Degraded, Docker, Tmpfs, TrafficCounter, Volume},
    local::Local,
    router::Router,
    ssh::Ssh,
};

mod docker;
mod local;
mod router;
pub mod ssh;

/// A host the test reaches from outside, through a program that runs commands on it (`docker exec`,
/// `ssh`). A [`Router`] runs on such a host.
pub trait Remote {
    /// `program args` on this host, as a local process that runs it there
    fn command(&self, program: &str, args: &[&str]) -> std::process::Command;
}

/// A host's identity. It stays the same while the host's address changes. Everything that refers to
/// a host without owning it holds its id. Two ids are equal if and only if they belong to the same
/// host.
#[derive(Clone)]
pub struct Id {
    /// what others need to know of the host, owned by its handles; its allocation is the host's
    /// identity
    shared: Weak<Shared>,
    /// names the host in messages, also once it's gone
    label: Arc<str>,
}

/// What a host's handles share with its [`Id`]s.
#[derive(Default)]
struct Shared {
    firewall: Mutex<Firewall>,
    /// its address on each network it's on, by the network's name
    addresses: Mutex<HashMap<String, IpAddr>>,
}

/// The faults on a host that drop its traffic. Each fault is a set of rules on the host. The rules
/// act on all of the host's networks, but never on traffic between things that run on the host
/// itself.
#[derive(Clone, Default)]
struct Firewall {
    /// whether it drops all traffic (see [`Docker::isolate`])
    isolated: bool,
    /// the hosts it drops the traffic to and from (see [`Docker::block`])
    blocked: HashSet<Id>,
}

impl Id {
    /// The host's firewall as it is now. The host must still exist: what refers to it must not
    /// outlive it.
    fn get_firewall(&self) -> Firewall {
        self.upgrade().lock_firewall().clone()
    }

    /// The address the host has now on the network named `network`, which it must be on. The
    /// host must still exist.
    pub(crate) fn get_address(&self, network: &str) -> IpAddr {
        *self
            .upgrade()
            .lock_addresses()
            .get(network)
            .unwrap_or_else(|| panic!("{self:?} is not on network `{network}`"))
    }

    /// whether the host is on the network named `network`; the host must still exist
    pub(crate) fn has_address(&self, network: &str) -> bool {
        self.upgrade().lock_addresses().contains_key(network)
    }

    fn upgrade(&self) -> Arc<Shared> {
        self.shared
            .upgrade()
            .unwrap_or_else(|| panic!("{self:?} was dropped while something still refers to it"))
    }
}

/// Whether the firewalls of the hosts `a` and `b` let the traffic between them through. Traffic
/// within one host always gets through.
pub(crate) fn firewalls_let_through(a: &Id, b: &Id) -> bool {
    a == b || {
        let (a_firewall, b_firewall) = (a.get_firewall(), b.get_firewall());
        !a_firewall.isolated
            && !b_firewall.isolated
            && !a_firewall.blocked.contains(b)
            && !b_firewall.blocked.contains(a)
    }
}

impl Shared {
    fn lock_firewall(&self) -> MutexGuard<'_, Firewall> {
        self.firewall.lock().expect("not poisoned")
    }

    fn lock_addresses(&self) -> MutexGuard<'_, HashMap<String, IpAddr>> {
        self.addresses.lock().expect("not poisoned")
    }
}

impl PartialEq for Id {
    fn eq(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.shared, &other.shared)
    }
}

impl Eq for Id {}

impl Hash for Id {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::ptr::hash(self.shared.as_ptr(), state);
    }
}

impl std::fmt::Debug for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "host `{}`", self.label)
    }
}
