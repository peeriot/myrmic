//! The networks a test's hosts sit on: what decides whether two hosts reach each other at all,
//! below the zenoh links an [`crate::overlay::Overlay`] wants between the runtimes on them.
//!
//! An overlay is created on one network ([`crate::overlay::Overlay::new`]) and asks it, for every
//! link it wants, whether the two hosts can reach each other. A network that the test can fault
//! (cut a host off, block two hosts from each other) answers according to the faults it has in
//! place; [`Local`], the test's own machine, never cuts anything.

use std::net::IpAddr;

/// A network hosts sit on, as an overlay sees it.
pub trait Network: Send + Sync {
    /// Whether traffic between the hosts at `a` and `b`, two different hosts on this network,
    /// gets through.
    fn reaches(&self, a: IpAddr, b: IpAddr) -> bool;
}

/// The test's own machine: its runtimes and nodes all reach each other, and nothing can cut them
/// apart.
#[derive(Clone, Copy)]
pub struct Local;

impl Network for Local {
    fn reaches(&self, _a: IpAddr, _b: IpAddr) -> bool {
        true
    }
}
