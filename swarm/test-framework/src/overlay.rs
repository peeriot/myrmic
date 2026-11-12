//! The zenoh links a test's runtimes form among themselves: which runtimes scout on which
//! multicast group, and which connect to which. An [`Overlay`] is the one place that knows this;
//! runtimes read their zenoh settings from it when they start, and a swarm reads from it which
//! links it expects.
//!
//! # Using it
//!
//! ```ignore
//! use test_framework::{host, in_process, network, overlay::Overlay};
//!
//! let overlay = Overlay::new(&network::Local);
//! let group = overlay.new_multicast_group();
//! let a = in_process::StoppedRt::new(&group).router_mode();  // a node that scouts on `group`
//! let myrmic = host::Local.myrmic_isolated(&overlay.connect_to(&a));
//! let b = myrmic.runtime();  // a CLI runtime whose one link is a connect to `a`
//! let session = zenoh::open(group.get_zenoh_config()).await.unwrap();  // the test, on the group
//! ```
//!
//! - An overlay sits on a network ([`crate::network`]), which decides whether two hosts reach
//!   each other at all. A link the overlay wants only exists where the network lets it.
//! - A runtime becomes a member of the overlay when it is created, by an [`Attachment`]: on a
//!   group, where it links to every other member of the group, or by a connect to another member
//!   ([`Overlay::connect_to`]), which is then its only link. A test's session, its CLI and a
//!   swarm attach the same way.
//! - [`Overlay::new_multicast_group`] hands out a group no other overlay uses, so runtimes of
//!   different overlays (e.g. of tests running in parallel) never see each other.
//! - [`Overlay::set_group`] moves a member to another group, for a runtime that should come back
//!   cut off from the rest; it takes effect at the member's next start.
//! - Groups and connects hold their overlay, so a test that only needs one of them need not keep
//!   the overlay.
//!
//! # Design notes
//!
//! The overlay is the zenoh view of a test's setup, sitting on the network the hosts sit on: a
//! link between two runtimes exists if the overlay wants it and the network lets their hosts
//! reach each other. Runtimes on the same host always reach each other. Runtimes don't keep their
//! group or connects themselves: they keep their membership, and ask the overlay for their
//! settings at start, so the overlay is the only source of who links to whom. A member's host
//! address is part of that topology, so the overlay holds it too: it is where a connect to the
//! member goes.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU16, Ordering},
    },
};

use crate::{network::Network, runtime::Stopped};

/// The zenoh links among a test's runtimes; see [the module docs](self). Clones are the same
/// overlay.
#[derive(Clone)]
pub struct Overlay {
    shared: Arc<Shared>,
}

/// What the clones of an overlay and its groups and connects share.
struct Shared {
    /// the network the overlay sits on
    network: Box<dyn Network>,
    state: Mutex<State>,
}

/// An overlay's groups and members.
struct State {
    /// the multicast group (`<address>:<port>`) of each [`GroupId`], at its index
    group_addresses: Vec<String>,
    /// at their [`Member`]'s index
    members: Vec<MemberState>,
}

/// What the overlay knows about one member.
struct MemberState {
    /// the address of the host it runs on, where a connect to it goes
    address: IpAddr,
    /// the group it scouts on; `None` for one attached by a connect
    group: Option<GroupId>,
    /// the members it connects to, by index
    connects: Vec<usize>,
    /// the port it listens on, set once a connect targets it
    listen_port: Option<u16>,
}

/// One of an overlay's multicast groups, by its index in [`State::group_addresses`].
#[derive(Clone, Copy, PartialEq, Eq)]
struct GroupId(usize);

/// A multicast group of an [`Overlay`]: the runtimes created on it scout on it, and so link to
/// each other. Clones are the same group.
#[derive(Clone)]
pub struct MulticastGroup {
    overlay: Overlay,
    id: GroupId,
}

/// A connect to one member of an [`Overlay`] (see [`Overlay::connect_to`]): what attaches by it
/// links to that member only. Clones are the same connect.
#[derive(Clone)]
pub struct Connect {
    target: Member,
}

/// How something attaches to an overlay: a runtime, a test's session or CLI, a swarm.
#[derive(Clone)]
pub enum Attachment {
    /// scouting on a group, linked to every member on it
    Group(MulticastGroup),
    /// by a connect to one member, linked to that member only
    Connect(Connect),
}

/// A member of an [`Overlay`]: a runtime, as the overlay knows it (see
/// [`crate::runtime::Stopped::get_overlay_member`]). Clones are the same member.
#[derive(Clone)]
pub struct Member {
    overlay: Overlay,
    index: usize,
}

impl Overlay {
    /// an overlay on `network`, with no group and no member yet
    pub fn new<N: Network + Clone + 'static>(network: &N) -> Self {
        Self {
            shared: Arc::new(Shared {
                network: Box::new(network.clone()),
                state: Mutex::new(State {
                    group_addresses: vec![],
                    members: vec![],
                }),
            }),
        }
    }

    /// a new multicast group of this overlay, which no other overlay uses
    pub fn new_multicast_group(&self) -> MulticastGroup {
        let mut state = self.lock();
        state.group_addresses.push(isolated_multicast_group());
        MulticastGroup {
            overlay: self.clone(),
            id: GroupId(state.group_addresses.len() - 1),
        }
    }

    /// A connect to the member `target`: what attaches by it links to `target` only. `target`
    /// listens for it from its next start on, so a runtime is connected to before it starts.
    ///
    /// The port it listens on is one that is free on the test's machine when the first connect
    /// to it is made.
    pub fn connect_to(&self, target: impl Into<Member>) -> Connect {
        let target = target.into();
        let index = self.index_of(&target);
        let mut state = self.lock();
        let member = &mut state.members[index];
        if member.listen_port.is_none() {
            member.listen_port = Some(free_port());
        }
        Connect { target }
    }

    /// Move the member `member` to `group`, one of this overlay's: from its next start on it
    /// scouts there, e.g. on a group nobody else is on, to come back cut off from the rest. A
    /// running member is not moved until it starts again.
    pub fn set_group(&self, member: impl Into<Member>, group: &MulticastGroup) {
        let member = member.into();
        assert!(
            Arc::ptr_eq(&group.overlay.shared, &self.shared),
            "the group belongs to another overlay"
        );
        let index = self.index_of(&member);
        self.lock().members[index].group = Some(group.id);
    }

    /// Whether the members `a` and `b` link to each other: the overlay wants it (the same group,
    /// or a connect between them) and the network lets their hosts reach each other.
    pub(crate) fn are_linked(&self, a: &Member, b: &Member) -> bool {
        let (a, b) = (self.index_of(a), self.index_of(b));
        let state = self.lock();
        let (member_a, member_b) = (&state.members[a], &state.members[b]);
        let same_group = member_a.group.is_some() && member_a.group == member_b.group;
        let connected = member_a.connects.contains(&b) || member_b.connects.contains(&a);
        (same_group || connected)
            && (member_a.address == member_b.address
                || self
                    .shared
                    .network
                    .reaches(member_a.address, member_b.address))
    }

    /// Whether what attaches by `attachment` reaches the member `member`: on a group, the
    /// members on it; by a connect, its target and what links to the target. The latter
    /// assumes the target passes traffic on for what attaches to it, as a router does.
    pub(crate) fn is_reachable_by(&self, attachment: &Attachment, member: &Member) -> bool {
        let index = self.index_of(member);
        let state = self.lock();
        let member = &state.members[index];
        match attachment {
            Attachment::Group(group) => member.group == Some(group.id),
            Attachment::Connect(connect) => {
                let target_index = self.index_of(&connect.target);
                let target = &state.members[target_index];
                index == target_index
                    || (target.group.is_some() && member.group == target.group)
                    || member.connects.contains(&target_index)
            }
        }
    }

    /// whether `member` is a member of this overlay
    pub(crate) fn has_member(&self, member: &Member) -> bool {
        Arc::ptr_eq(&member.overlay.shared, &self.shared)
    }

    /// `member`'s index among this overlay's members
    fn index_of(&self, member: &Member) -> usize {
        assert!(
            self.has_member(member),
            "{member:?} is not a member of this overlay"
        );
        member.index
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared.state.lock().expect("not poisoned")
    }

    /// a new member on the host at `address`, scouting on `group` and connecting to `connects`
    fn add_member(&self, address: IpAddr, group: Option<GroupId>, connects: Vec<usize>) -> Member {
        let mut state = self.lock();
        state.members.push(MemberState {
            address,
            group,
            connects,
            listen_port: None,
        });
        Member {
            overlay: self.clone(),
            index: state.members.len() - 1,
        }
    }
}

impl Member {
    /// the overlay this is a member of
    pub fn get_overlay(&self) -> &Overlay {
        &self.overlay
    }

    /// The `zenoh` section of this member's config, as of now: the group it scouts on
    /// (multicast scouting off for one attached by a connect), the members it connects to, and
    /// the port it listens on (any free one if no connect targets it).
    pub(crate) fn get_zenoh_settings(&self) -> serde_json::Value {
        let index = self.overlay.index_of(self);
        let state = self.overlay.lock();
        let member = &state.members[index];
        let scouting = match member.group {
            Some(GroupId(group)) => serde_json::json!({ "address": state.group_addresses[group] }),
            None => serde_json::json!({ "enabled": false }),
        };
        let connects: Vec<String> = member
            .connects
            .iter()
            .map(|&target| endpoint(&state.members[target]))
            .collect();
        let listen_port = member.listen_port.unwrap_or(0);
        serde_json::json!({
            "scouting": { "multicast": scouting },
            "connect": { "endpoints": connects },
            "listen": { "endpoints": [format!("tcp/[::]:{listen_port}")] },
        })
    }
}

impl PartialEq for Member {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.overlay.shared, &other.overlay.shared) && self.index == other.index
    }
}

impl Eq for Member {}

/// a runtime's member, e.g. `overlay.connect_to(&rt)`
impl<S: Stopped> From<&S> for Member {
    fn from(rt: &S) -> Self {
        rt.get_overlay_member().clone()
    }
}

impl From<&Member> for Member {
    fn from(member: &Member) -> Self {
        member.clone()
    }
}

impl std::fmt::Debug for Member {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "overlay member {}", self.index)
    }
}

impl MulticastGroup {
    /// the overlay this group belongs to
    pub fn get_overlay(&self) -> &Overlay {
        &self.overlay
    }

    /// the group's address (`<address>:<port>`), e.g. for the CLI's `--multicast-group`
    pub fn get_address(&self) -> String {
        let GroupId(group) = self.id;
        self.overlay.lock().group_addresses[group].clone()
    }

    /// A zenoh peer config that scouts on this group, for a test's own session: it finds every
    /// runtime on the group the way they find each other. The session is no member of the
    /// overlay, so no swarm expects links to it.
    pub fn get_zenoh_config(&self) -> zenoh::Config {
        let mut config = zenoh::Config::default();
        config
            .set_mode(Some(zenoh::config::WhatAmI::Peer))
            .expect("setting zenoh mode cannot fail");
        config
            .insert_json5(
                "scouting/multicast/address",
                &serde_json::json!(self.get_address()).to_string(),
            )
            .expect("a multicast group is a valid scouting address");
        config
    }
}

impl Connect {
    /// the overlay this connect belongs to
    pub fn get_overlay(&self) -> &Overlay {
        self.target.get_overlay()
    }

    /// the zenoh endpoint of the connect's target, e.g. for the CLI's `--connect`
    pub fn get_target_endpoint(&self) -> String {
        let overlay = self.get_overlay();
        endpoint(&overlay.lock().members[overlay.index_of(&self.target)])
    }

    /// A zenoh client config that connects to the target only, for a test's own session. The
    /// session is no member of the overlay, so no swarm expects links to it. Its open fails unless
    /// the target already runs.
    pub fn get_zenoh_config(&self) -> zenoh::Config {
        let mut config = zenoh::Config::default();
        config
            .set_mode(Some(zenoh::config::WhatAmI::Client))
            .expect("setting zenoh mode cannot fail");
        config
            .insert_json5(
                "connect/endpoints",
                &serde_json::json!([self.get_target_endpoint()]).to_string(),
            )
            .expect("a connect endpoint is valid");
        config
            .insert_json5("scouting/multicast/enabled", "false")
            .expect("disabling multicast scouting cannot fail");
        config
    }
}

impl Attachment {
    /// the overlay this attaches to
    pub fn get_overlay(&self) -> &Overlay {
        match self {
            Self::Group(group) => group.get_overlay(),
            Self::Connect(connect) => connect.get_overlay(),
        }
    }

    /// a zenoh config for a test's own session attached this way (see
    /// [`MulticastGroup::get_zenoh_config`], [`Connect::get_zenoh_config`])
    pub fn get_zenoh_config(&self) -> zenoh::Config {
        match self {
            Self::Group(group) => group.get_zenoh_config(),
            Self::Connect(connect) => connect.get_zenoh_config(),
        }
    }

    /// the `myrmic` arguments that have a CLI call find the runtimes this way
    pub(crate) fn get_cli_args(&self) -> [String; 2] {
        match self {
            Self::Group(group) => ["--multicast-group".to_owned(), group.get_address()],
            Self::Connect(connect) => ["--connect".to_owned(), connect.get_target_endpoint()],
        }
    }

    /// Make a new member of the overlay on the host at `address`, attached this way; what a
    /// runtime calls when it is created.
    pub(crate) fn add_member(&self, address: IpAddr) -> Member {
        match self {
            Self::Group(group) => group.overlay.add_member(address, Some(group.id), vec![]),
            Self::Connect(connect) => {
                let overlay = connect.get_overlay();
                overlay.add_member(address, None, vec![overlay.index_of(&connect.target)])
            }
        }
    }
}

impl From<&MulticastGroup> for Attachment {
    fn from(group: &MulticastGroup) -> Self {
        Self::Group(group.clone())
    }
}

impl From<&Connect> for Attachment {
    fn from(connect: &Connect) -> Self {
        Self::Connect(connect.clone())
    }
}

/// The address of the test's own machine, for what runs on it: the endpoint a connect to one of
/// its members goes to.
pub(crate) const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// the zenoh endpoint `member` listens on; it must have a listen port, i.e. be a connect's target
fn endpoint(member: &MemberState) -> String {
    let port = member
        .listen_port
        .expect("a connect's target has a listen port from the connect on");
    format!("tcp/{}", SocketAddr::new(member.address, port))
}

/// A TCP port free on the test's machine right now. Another process may take it before the
/// member listens on it; the member's start then fails.
fn free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .and_then(|listener| listener.local_addr())
        .expect("failed to find a free port")
        .port()
}

/// A multicast group no other overlay uses: the address from this process's pid
/// (administratively scoped 239.0.0.0/8; its low 24 bits cover every Linux pid), the port from a
/// per-process counter. The counter starts above 7446, the port of sorg-tests' per-process group
/// on the same address (`sorg_tests::scope_test_multicast`), which in-process tests in the same
/// binary use.
fn isolated_multicast_group() -> String {
    static NEXT_PORT: AtomicU16 = AtomicU16::new(FIRST_PORT);
    let port = NEXT_PORT.fetch_add(1, Ordering::Relaxed);
    assert!(
        port >= FIRST_PORT,
        "ran out of ports for isolated multicast groups"
    );
    let pid = std::process::id();
    format!(
        "239.{}.{}.{}:{port}",
        (pid >> 16) & 0xFF,
        (pid >> 8) & 0xFF,
        pid & 0xFF
    )
}

/// the port of a process's first overlay group: one above sorg-tests' group (see
/// [`isolated_multicast_group`])
const FIRST_PORT: u16 = 7447;
