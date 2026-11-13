//! The networks a test's hosts sit on: what decides whether two hosts reach each other at all,
//! below the zenoh links an [`crate::overlay::Overlay`] wants between the runtimes on them, and
//! at which address each host is.
//!
//! An overlay is created on one network ([`crate::overlay::Overlay::new`]) and asks it, for every
//! link it wants, whether the two hosts can reach each other, and for a connect's target, where
//! it is. A network that the test can fault (cut a host off, block two hosts from each other)
//! answers according to the faults it has in place; [`Local`], the test's own machine, never
//! cuts anything, and [`Docker`] cuts what the test tells it to.
//!
//! # Design notes
//!
//! A network creates its hosts ([`Docker::new_host`]) and is the one place that knows their
//! addresses, the test's own machine's included. Everything else knows a host by its
//! [`HostId`], which stays when its address changes ([`Docker::readdress`]), so nothing holds a
//! copy that could go stale.

use std::{
    net::{IpAddr, Ipv4Addr},
    sync::atomic::{AtomicU64, Ordering},
};

pub use self::docker::{Docker, TEST_CONTAINER};

mod docker;

/// A network hosts sit on, as an overlay sees it.
pub trait Network: Send + Sync {
    /// Whether traffic between the hosts `a` and `b`, two different hosts on this network, gets
    /// through.
    fn reaches(&self, a: HostId, b: HostId) -> bool;

    /// The address `host`, a host of this network, has now: where a connect to what runs on it
    /// goes.
    fn get_address(&self, host: HostId) -> IpAddr;
}

/// A host's identity, which stays while its address may change. Unique in the process, so a host
/// of one network is never taken for a host of another.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct HostId(u64);

impl HostId {
    /// The test's own machine, a host of every network, each of which knows its address on it.
    pub const TEST_MACHINE: HostId = HostId(0);

    /// an id no other host has
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// The test's own machine, the only host: its runtimes and nodes all reach each other at
/// localhost, and nothing can cut them apart.
#[derive(Clone, Copy)]
pub struct Local;

impl Network for Local {
    fn reaches(&self, _a: HostId, _b: HostId) -> bool {
        true
    }

    fn get_address(&self, host: HostId) -> IpAddr {
        assert_eq!(
            host,
            HostId::TEST_MACHINE,
            "network::Local has no host but the test's machine"
        );
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    }
}
