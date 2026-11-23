//! Host sets: groups of a test's hosts that can dial each other, and how they dial. Together, in a
//! [`World`](super::World), the host sets tell an [`crate::overlay::Overlay`] everything it needs
//! to know about the network below the zenoh links it wants between its runtimes.
//!
//! A host set need not be a LAN. It is a set of hosts that dial each other directly, whatever
//! lies in between (switches, routers, a VPN). A host may be in several sets. A set may carry
//! multicast (a LAN, a docker network) or not (a routed network). A test never deals with
//! addresses and ports: a set reads them from its hosts to find out how one host dials another.
//!
//! For every link an overlay wants, it asks its world whether a route from one host to the other
//! exists, and for a connect, what that route is. The world asks the sets both hosts are in.
//! Firewalls are no concern of a set ([`host::Docker::isolate`], [`host::Docker::block`]): a route
//! through a set exists even while a firewall drops the traffic on it.
//!
//! # Design notes
//!
//! A route is a zenoh endpoint. A route from a host to itself is its loopback. Routes are one hop
//! long: a set has no route between two hosts unless both are in it, so there is no route that
//! passes through a third host that is in several sets. Getting from one host to another through
//! a third host is the overlay's business, through zenoh routers on the way.
//!
//! [`ConnectedHostSet::find_route`] is the only method a set implements. Only this crate can call
//! it, because its last parameter has a type no other crate can name. Callers use
//! [`ConnectedHostSet::get_route`] and [`ConnectedHostSet::route_exists`] instead. The two always
//! agree, because both are built on `find_route`.

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

    /// How `from` dials `port` on `to` through this set, where both hosts are in it, or `None` if
    /// it can't. Port 0 stands for whatever port the system on `to` picks; the route then ends in
    /// `:0`, which only says that a route exists. Implement this method, but call
    /// [`Self::get_route`] or [`Self::route_exists`].
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

/// How `from` dials `port` on `to` in the host set named `host_set`, whose hosts dial each other at
/// their addresses on it: at `to`'s address, or on the loopback if both are the same host. `from`
/// must be in the set.
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
