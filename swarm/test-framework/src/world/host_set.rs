//! The sets of hosts a test's hosts form: which hosts can dial each other, and how. Together, in
//! a [`World`](super::World), they are all an [`crate::overlay::Overlay`] needs to know below the
//! zenoh links it wants between the runtimes on them.
//!
//! A host set is not a LAN: it is a set of hosts that dial each other directly, whatever lies in
//! between (switches, routers, a VPN). A host may be in several sets, and a set may carry
//! multicast (a LAN, a docker network) or not (a routed network). Addresses and ports are no
//! concern of the test: a set reads them from its hosts to tell how one dials another.
//!
//! An overlay asks its world, and the world the sets both hosts are in, for every link it wants,
//! whether a route from one host to the other exists, and for a connect, the route itself. The
//! faults on the hosts that drop their traffic, their firewalls
//! ([`host::Docker::isolate`], [`host::Docker::block`]), are no concern of a set: a route through
//! it exists also while a firewall drops the traffic on it.
//!
//! # Design notes
//!
//! A route is a zenoh endpoint, and a route from a host to itself is its loopback. Routes go one
//! hop: a set has no route between two hosts that are not both in it, so a route that would
//! cross a host in several sets doesn't exist. What gets from one host to another through a
//! third is the overlay's business, through the zenoh routers on the way.
//!
//! [`ConnectedHostSet::find_route`] is the one method a set implements, and only this crate can
//! call it (its last parameter has a type no other crate can name): [`ConnectedHostSet::get_route`] and
//! [`ConnectedHostSet::route_exists`] are the ones to call, and agree with each other because
//! both come from it.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    num::NonZeroU16,
};

use zenoh::config::EndPoint;

use crate::host;

pub use self::{
    docker::{Docker, TEST_CONTAINER},
    ip::Ip,
};

mod docker;
mod ip;

/// A set of hosts that dial each other directly, as an overlay sees it; see
/// [the module docs](self).
pub trait ConnectedHostSet: Send + Sync {
    /// whether `host` is in this set
    fn has_host(&self, host: &host::Id) -> bool;

    /// whether multicast gets from each of its hosts to the others, so the runtimes on them find
    /// each other by scouting
    fn has_multicast(&self) -> bool;

    /// How `from` dials `port` on `to` through this set, both hosts of it: `None` if it can't.
    /// Port 0 stands for whatever port the system on `to` picks; the route then ends in `:0`, which
    /// tells only that one exists. Implement it, but call [`Self::get_route`] or
    /// [`Self::route_exists`].
    fn find_route(
        &self,
        from: &host::Id,
        to: &host::Id,
        port: u16,
        _: sealed::Token,
    ) -> Option<EndPoint>;

    /// how `from` dials `port` on `to` through this set, both hosts of it: `None` if it can't
    fn get_route(&self, from: &host::Id, to: &host::Id, port: NonZeroU16) -> Option<EndPoint> {
        self.find_route(from, to, port.get(), sealed::Token)
    }

    /// whether `from` can dial `port` on `to` through this set, both hosts of it; port 0 stands
    /// for whatever port the system on `to` picks
    fn route_exists(&self, from: &host::Id, to: &host::Id, port: u16) -> bool {
        self.find_route(from, to, port, sealed::Token).is_some()
    }
}

/// What keeps [`ConnectedHostSet::find_route`] to this crate.
mod sealed {
    /// A value only this crate can name, so only it can call
    /// [`super::ConnectedHostSet::find_route`].
    pub struct Token;
}

/// The test's own machine alone: its runtimes and nodes all reach each other on its loopback,
/// and nothing can cut them apart.
#[derive(Clone, Copy)]
pub struct Local;

impl ConnectedHostSet for Local {
    fn has_host(&self, host: &host::Id) -> bool {
        *host == host::get_local_id()
    }

    fn has_multicast(&self) -> bool {
        true
    }

    fn find_route(
        &self,
        from: &host::Id,
        to: &host::Id,
        port: u16,
        _: sealed::Token,
    ) -> Option<EndPoint> {
        assert!(
            self.has_host(from) && self.has_host(to),
            "host_set::Local has no host but the test's machine, asked for {from:?} to {to:?}"
        );
        Some(tcp_endpoint(IpAddr::V4(Ipv4Addr::LOCALHOST), port))
    }
}

/// How `from` dials `port` on `to` in the host set named `host_set`, whose hosts dial each other
/// at their addresses on it: at `to`'s, or on the loopback if both are the same host. `from` must
/// be in the set.
fn route_by_address(host_set: &str, from: &host::Id, to: &host::Id, port: u16) -> EndPoint {
    assert!(
        from.has_address(host_set),
        "{from:?} is not in host set `{host_set}`"
    );
    let address = if from == to {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        to.get_address(host_set)
    };
    tcp_endpoint(address, port)
}

/// the zenoh endpoint of `port` at `address` over TCP
fn tcp_endpoint(address: IpAddr, port: u16) -> EndPoint {
    format!("tcp/{}", SocketAddr::new(address, port))
        .parse()
        .expect("a TCP socket address is a valid zenoh endpoint")
}
