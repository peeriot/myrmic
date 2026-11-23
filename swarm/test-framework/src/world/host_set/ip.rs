use std::{
    net::{IpAddr, Ipv4Addr, ToSocketAddrs},
    sync::Arc,
};

use netlink_packet_core::{NLM_F_REQUEST, NetlinkHeader, NetlinkMessage, NetlinkPayload};
use netlink_packet_route::{
    AddressFamily, RouteNetlinkMessage,
    route::{RouteAddress, RouteAttribute, RouteMessage},
};
use netlink_sys::{Socket, SocketAddr, protocols::NETLINK_ROUTE};
use zenoh::config::EndPoint;

use super::{ConnectedHostSet, route_by_address, sealed};
use crate::host;

/// A plain IP network of machines that exist on their own, e.g. the Raspberry Pis of the rack. Each
/// machine is dialled at its address on the network: the address its SSH destination's name
/// resolves to. [`Self::lan`] carries multicast, [`Self::routed`] doesn't. Clones are the same host
/// set.
#[derive(Clone)]
pub struct Ip {
    shared: Arc<IpShared>,
}

/// What the clones of an [`Ip`] host set share.
struct IpShared {
    /// names it in its hosts' addresses
    name: String,
    multicast: bool,
}

impl Ip {
    /// A LAN of `hosts`: they dial each other directly, and multicast gets between them.
    pub fn lan(hosts: &[&host::Ssh]) -> Self {
        let ip = Self::new(true);
        for host in hosts {
            host.set_address(&ip.shared.name, IpAddr::V4(resolve(host.get_destination())));
        }
        ip
    }

    /// A routed network of the test's own machine and `hosts`, e.g. over a VPN: they dial each
    /// other through routers, and multicast doesn't get through those. The test's machine is on it
    /// at the address from which its system sends to the hosts.
    pub fn routed(hosts: &[&host::Ssh]) -> Self {
        let ip = Self::new(false);
        let addresses: Vec<Ipv4Addr> = hosts
            .iter()
            .map(|host| resolve(host.get_destination()))
            .collect();
        for (host, address) in hosts.iter().zip(&addresses) {
            host.set_address(&ip.shared.name, IpAddr::V4(*address));
        }
        let first = addresses
            .first()
            .expect("the test's machine needs a host to be routed to");
        host::set_local_address(&ip.shared.name, IpAddr::V4(get_source_address(*first)));
        ip
    }

    fn new(multicast: bool) -> Self {
        Self {
            shared: Arc::new(IpShared {
                name: format!("ip-{}", uuid::Uuid::new_v4().simple()),
                multicast,
            }),
        }
    }
}

impl ConnectedHostSet for Ip {
    fn has_host(&self, host: &host::Id) -> bool {
        host.has_address(&self.shared.name)
    }

    fn has_multicast(&self) -> bool {
        self.shared.multicast
    }

    fn find_route(
        &self,
        from: &host::Id,
        to: &host::Id,
        port: u16,
        _: sealed::Token,
    ) -> Option<EndPoint> {
        Some(route_by_address(&self.shared.name, from, to, port))
    }
}

impl Drop for IpShared {
    fn drop(&mut self) {
        // only a routed network has the test's machine on it
        if host::get_local_id().has_address(&self.name) {
            host::remove_local_address(&self.name);
        }
    }
}

/// the `IPv4` address the name of the SSH destination `destination` (`[user@]name`) resolves to
fn resolve(destination: &str) -> Ipv4Addr {
    let name = destination
        .rsplit_once('@')
        .map_or(destination, |(_, name)| name);
    (name, 0)
        .to_socket_addrs()
        .unwrap_or_else(|err| panic!("failed to resolve `{name}`: {err}"))
        .find_map(|address| match address.ip() {
            IpAddr::V4(address) => Some(address),
            IpAddr::V6(_) => None,
        })
        .unwrap_or_else(|| panic!("`{name}` has no IPv4 address"))
}

/// The address this machine sends from to `destination`: the kernel's routing table answers it,
/// as `ip route get` asks it.
fn get_source_address(destination: Ipv4Addr) -> Ipv4Addr {
    let mut socket = Socket::new(NETLINK_ROUTE).expect("failed to open a netlink socket");
    socket.bind_auto().expect("failed to bind a netlink socket");
    socket
        .connect(&SocketAddr::new(0, 0))
        .expect("failed to connect a netlink socket to the kernel");

    // a destination and no dump flag: the route to that one address
    let mut query = RouteMessage::default();
    query.header.address_family = AddressFamily::Inet;
    query.header.destination_prefix_length = 32;
    query
        .attributes
        .push(RouteAttribute::Destination(RouteAddress::Inet(destination)));
    let mut header = NetlinkHeader::default();
    header.flags = NLM_F_REQUEST;
    let mut request = NetlinkMessage::new(
        header,
        NetlinkPayload::from(RouteNetlinkMessage::GetRoute(query)),
    );
    request.finalize();
    let mut buffer = vec![0; request.buffer_len()];
    request.serialize(&mut buffer);
    socket
        .send(&buffer, 0)
        .expect("failed to send a route query to the kernel");

    let mut reply = vec![0; 8192];
    let size = socket
        .recv(&mut &mut reply[..], 0)
        .expect("failed to receive the kernel's route");
    let reply = NetlinkMessage::<RouteNetlinkMessage>::deserialize(&reply[..size])
        .expect("the kernel answers with a netlink message");
    match reply.payload {
        NetlinkPayload::InnerMessage(RouteNetlinkMessage::NewRoute(route)) => route
            .attributes
            .iter()
            .find_map(|attribute| match attribute {
                RouteAttribute::PrefSource(RouteAddress::Inet(source)) => Some(*source),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the kernel's route to {destination} has no source address")),
        NetlinkPayload::Error(err) => panic!("the kernel has no route to {destination}: {err:?}"),
        other => panic!("the kernel answered a route query with {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    /// The kernel's route to the loopback leaves from the loopback, as `ip route get 127.0.0.1`
    /// shows: the query and its answer get through the netlink encoding.
    #[test]
    fn the_route_to_the_loopback_leaves_from_it() {
        assert_eq!(
            super::get_source_address(Ipv4Addr::LOCALHOST),
            Ipv4Addr::LOCALHOST
        );
    }
}
