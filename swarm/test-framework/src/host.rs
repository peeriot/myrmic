//! The machines a test's runtimes run on: the test's own machine ([`Local`]), a docker container
//! ([`Docker`]), a machine reached over SSH ([`Ssh`]). A host hands out the CLI shims that run on
//! it ([`crate::myrmic::Myrmic`]), and through them the runtimes.
//!
//! # Using it
//!
//! ```ignore
//! use test_framework::{docker, host, host_set, overlay::Overlay, world};
//!
//! let group = Overlay::new(&World::local()).new_multicast_group();
//! let myrmic = host::Local.myrmic_isolated(&group);  // the CLI on the test's machine
//!
//! let host_set = host_set::Docker::new(&docker::Daemon::connect()).await;
//! let group = Overlay::new(&world![host_set]).new_multicast_group();
//! let container = host_set.new_host().await;
//! let remote = container.myrmic(&group);  // the CLI in a container
//! ```
//!
//! # Design notes
//!
//! A host is not the same as a shim's backend ([`crate::myrmic::MyrmicBackend`]): the backend
//! only knows how to run a command there, while a host is the machine: its identity ([`Id`]),
//! the shims it carries (one per attachment or isolation), its faults and its address on each
//! network it's on, which a fault can change ([`Docker::readdress`]).
//!
//! A host's handle ([`Local`], [`Docker`], [`Ssh`]) owns it, and is what a test works with. Everything
//! that refers to a host without owning it (an overlay's members, a host set) holds its [`Id`]:
//! it doesn't keep the host alive, and reads through it what others need to know of the host:
//! its firewall, the faults on it that drop its traffic, and its addresses. Addresses are no
//! concern of a test: a host set reads them to tell how another host dials this one.

use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    net::IpAddr,
    sync::{Arc, Mutex, MutexGuard, Weak},
};

pub use self::{docker::Docker, local::Local, router::Router, ssh::Ssh};
pub(crate) use self::{
    docker::{DOCKER_DATA_DIR, DOCKER_DATA_DIR_OPTIONS},
    local::{get_local_id, remove_local_address, set_local_address},
};

mod docker;
mod local;
mod router;
mod ssh;

/// A host the test reaches from the outside, through a program that runs commands there
/// (`docker exec`, `ssh`): what a [`Router`] runs on.
pub trait Remote {
    /// `program args` on this host, as a local process that runs it there
    fn command(&self, program: &str, args: &[&str]) -> std::process::Command;
}

/// A host's identity, which stays while its address may change, held by what refers to the host
/// without owning it. Two ids are equal iff they are of the same host.
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

/// The faults on a host that drop its traffic. Each is a set of rules on the host, which act on
/// all its networks, but never on its traffic with itself.
#[derive(Clone, Default)]
pub(crate) struct Firewall {
    /// whether it drops all traffic (see [`Docker::isolate`])
    pub(crate) isolated: bool,
    /// the hosts it drops the traffic to and from (see [`Docker::block`])
    pub(crate) blocked: HashSet<Id>,
}

impl Id {
    /// The host's firewall as it is now. The host must still exist: what refers to it must not
    /// outlive it.
    pub(crate) fn get_firewall(&self) -> Firewall {
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

/// Whether the hosts `a` and `b` let the traffic between them through their firewalls. A host's
/// own traffic, between what runs on it, always gets through.
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
