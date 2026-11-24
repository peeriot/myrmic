//! The zenoh links between a test's runtimes: which runtimes scout on which multicast group, and
//! which runtime connects to which. An [`Overlay`] is the only place that knows this. Runtimes
//! read their zenoh settings from it when they start, and a swarm reads from it which links to
//! expect.
//!
//! # Using it
//!
//! ```ignore
//! use test_framework::{World, host, in_process, overlay::Overlay};
//!
//! let overlay = Overlay::new(&World::local());
//! let group = overlay.new_multicast_group();
//! let a = in_process::StoppedRt::new(&group).router_mode(); // a node that scouts on `group`
//! let myrmic = host::Local.myrmic_isolated(overlay.connect_to(&a));
//! let b = myrmic.runtime(); // a CLI runtime whose only link is a connect to `a`
//! let session = zenoh::open(group.get_zenoh_config()).await.unwrap(); // the test, on the group
//! ```
//!
//! - An overlay sits on a world ([`crate::World`]). The world's host sets decide whether one host
//!   can dial another at all, and the hosts' firewalls decide whether the traffic gets through. A
//!   link the overlay wants only exists where both allow it.
//! - A runtime becomes a member of the overlay when it is created. How it attaches is an
//!   [`Attachment`]: either on a group, where it links to every other member of the group, or by
//!   a connect to another member ([`Overlay::connect_to`]), which is then its only link. A test's
//!   session, its CLI and a swarm attach in the same two ways.
//! - [`Overlay::new_multicast_group`] returns a group that no other overlay uses. So runtimes of
//!   different overlays, e.g. of tests that run in parallel, never see each other.
//! - [`Overlay::set_group`] moves a member to another group, e.g. so that a runtime comes back
//!   cut off from the others. The move takes effect when the member starts the next time.
//! - [`Overlay::set_listen_port`] sets the port a member listens on. This is for something
//!   outside the overlay that dials a fixed address, e.g. an embedded runtime whose firmware has
//!   the address built in.
//! - Groups and members keep their overlay alive, so a test that only needs a group or a member
//!   doesn't have to keep the overlay.
//!
//! # Design notes
//!
//! Three things decide whether two runtimes are linked: the overlay must want the link, the
//! world must have a route from the host of the dialing runtime to the host of the other, and
//! the firewalls of both hosts must let the traffic through.
//!
//! Runtimes don't store their group or their connects themselves. They store their membership,
//! and ask the overlay for their settings when they start, so the overlay is the only source of
//! who links to whom. A member's host is part of that picture, so the overlay holds the host too,
//! by its id. The route for a connect to a member is asked from the world each time a connect is
//! built, because a fault may have moved the member's host.
//!
//! The links the overlay reports are the links zenoh actually has, not the ones the settings ask
//! for. A link that a firewall cuts doesn't exist, but the settings keep the route to it, so the
//! link comes back once the cut is lifted.

use std::{
    collections::HashSet,
    net::{Ipv4Addr, TcpListener},
    num::NonZeroU16,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU16, Ordering},
    },
};

use zenoh::config::{EndPoint, WhatAmI};

use crate::{World, host, runtime::Stopped};

/// The zenoh links among a test's runtimes; see [the module docs](self). Clones are the same
/// overlay.
#[derive(Clone)]
pub struct Overlay {
    shared: Arc<Shared>,
}

/// What the clones of an overlay and its groups and members share.
struct Shared {
    /// the world the overlay sits on
    world: World,
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
    /// the host it runs on
    host: host::Id,
    /// the group it scouts on; `None` for one attached by a connect
    group: Option<GroupId>,
    /// the members it connects to, by index
    connects: Vec<usize>,
    /// the port it listens on: set by the test, or a free one once a connect targets it
    listen_port: Option<NonZeroU16>,
    /// The mode the member is up in, or `None` while it isn't up: what its last start reported,
    /// until its running handle went away (see [`Member::come_up`]). A router passes traffic on
    /// between the members linked to it. The mode itself is a setting of the runtime.
    up: Option<WhatAmI>,
}

impl MemberState {
    fn is_router(&self) -> bool {
        self.up == Some(WhatAmI::Router)
    }
}

/// One of an overlay's multicast groups, by its index in [`State::group_addresses`].
#[derive(Clone, Copy, PartialEq, Eq)]
struct GroupId(usize);

/// A multicast group of an [`Overlay`]. The runtimes created on it scout on it, and so link to each
/// other. Clones are the same group.
#[derive(Clone)]
pub struct MulticastGroup {
    overlay: Overlay,
    id: GroupId,
}

/// How something attaches to an overlay: a runtime, a test's session or CLI, a swarm.
#[derive(Clone)]
pub enum Attachment {
    /// scouting on a group, linked to every member on it
    Group(MulticastGroup),
    /// by a connect to one member, linked to that member only (see [`Overlay::connect_to`])
    ConnectTo(Member),
}

/// A member of an [`Overlay`]: a runtime, as the overlay sees it (see
/// [`crate::runtime::Stopped::get_overlay_member`]). Clones are the same member.
#[derive(Clone)]
pub struct Member {
    overlay: Overlay,
    index: usize,
}

impl Overlay {
    /// an overlay on `world`, with no group and no member yet
    pub fn new(world: &World) -> Self {
        Self {
            shared: Arc::new(Shared {
                world: world.clone(),
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

    /// An attachment by a connect to the member `target`: what attaches this way links to `target`
    /// only. `target` listens for the connect from its next start on, so something can attach to a
    /// runtime before the runtime starts. Panics if `target` runs and doesn't listen yet (no
    /// earlier connect targeted it before its start): it would listen only after a restart.
    pub fn connect_to(&self, target: impl Into<Member>) -> Attachment {
        let target = target.into();
        assert!(
            self.has_member(&target),
            "{target:?} is not a member of this overlay"
        );
        target.ensure_listen_port();
        Attachment::ConnectTo(target)
    }

    /// Move the member `member` to `group`, a group of this overlay. From its next start on, the
    /// member scouts on `group`, e.g. on a group nobody else is on, so that it comes back cut off
    /// from the others. A member that is up only moves when it starts again.
    pub fn set_group(&self, member: impl Into<Member>, group: &MulticastGroup) {
        let member = member.into();
        assert!(
            Arc::ptr_eq(&group.overlay.shared, &self.shared),
            "the group belongs to another overlay"
        );
        let index = self.index_of(&member);
        self.lock().members[index].group = Some(group.id);
    }

    /// Make the member `member` listen on `port`. From its next start on it listens there, and the
    /// members that connect to it dial that port from their own next start on. This is for
    /// something outside the overlay that dials a fixed address, e.g. an embedded runtime whose
    /// firmware has the address built in; the overlay's own connects work with any port.
    pub fn set_listen_port(&self, member: impl Into<Member>, port: NonZeroU16) {
        let index = self.index_of(&member.into());
        self.lock().members[index].listen_port = Some(port);
    }

    /// Whether the members `a` and `b` are linked: directly, by a shared group or a connect, or by
    /// gossip. Gossip links every peer of a component to every member of that component that the
    /// peer's host can dial or be dialled from (see [`Self::links`]).
    pub(crate) fn are_linked(&self, a: &Member, b: &Member) -> bool {
        let (a, b) = (self.index_of(a), self.index_of(b));
        self.links(&self.lock(), a, b)
    }

    /// Whether what attaches by `attachment` hears from the member `member`. This is a graph search
    /// that starts from what the attachment is linked to and continues through the members that
    /// pass traffic on.
    ///
    /// On a group, the test's session is a peer on the test's machine. Gossip links it to every
    /// member of the group's component whose host the test's machine can dial or be dialled from.
    /// By a connect, the session is a client of the target only, and the target passes traffic on
    /// for it, as every peer does for its clients. Beyond the target, only routers pass traffic on.
    pub(crate) fn is_reachable_by(&self, attachment: &Attachment, member: &Member) -> bool {
        let wanted = self.index_of(member);
        let state = self.lock();
        let (first, passes_on_for_session) = match attachment {
            Attachment::Group(group) => {
                let local = host::get_local_id();
                // linked directly: the members up on the group that the session finds by scouting
                let scouted = (0..state.members.len()).filter(|&index| {
                    let member = &state.members[index];
                    member.up.is_some()
                        && member.group == Some(group.id)
                        && self.scouts(&local, &member.host)
                });
                // and by gossip, as any peer: every member of their component that the test's
                // machine can dial or be dialled from
                let first: Vec<usize> = self
                    .get_component(&state, scouted)
                    .into_iter()
                    .filter(|&index| {
                        let host = &state.members[index].host;
                        self.scouts(&local, host) || self.dial_either_way(&local, host)
                    })
                    .collect();
                (first, None)
            }
            Attachment::ConnectTo(target) => {
                let target = self.index_of(target);
                (vec![target], Some(target))
            }
        };
        let mut reached: HashSet<usize> = first.iter().copied().collect();
        let mut queue = first;
        while let Some(index) = queue.pop() {
            let passes_on =
                state.members[index].is_router() || passes_on_for_session == Some(index);
            if passes_on {
                for other in 0..state.members.len() {
                    if !reached.contains(&other) && self.links(&state, index, other) {
                        reached.insert(other);
                        queue.push(other);
                    }
                }
            }
        }
        reached.contains(&wanted)
    }

    /// Whether the members at `a` and `b` are linked: directly ([`Self::is_direct_link`]) or by
    /// gossip. Zenoh peers tell each other about their links, and a peer connects to every peer and
    /// router it learns of. So within a component (the members joined by direct links), two members
    /// are linked if the host of one can dial the host of the other. Two routers are the exception:
    /// routers never connect to anything on their own.
    fn links(&self, state: &State, a: usize, b: usize) -> bool {
        let (member_a, member_b) = (&state.members[a], &state.members[b]);
        let both_routers = member_a.is_router() && member_b.is_router();
        a != b
            && (self.is_direct_link(state, a, b)
                || (!both_routers
                    // first: only members that are up share a component, and only their hosts
                    // surely still exist
                    && self.get_component(state, [a]).contains(&b)
                    && self.dial_either_way(&member_a.host, &member_b.host)))
    }

    /// Whether the members at `a` and `b`, both up, form a link of their own, without gossip.
    /// That is either a connect from one to the other that its host can dial, or a shared group
    /// where multicast gets between their hosts. Two routers don't link on a shared group, because
    /// routers don't connect to what they find there.
    fn is_direct_link(&self, state: &State, a: usize, b: usize) -> bool {
        let (member_a, member_b) = (&state.members[a], &state.members[b]);
        let both_up = member_a.up.is_some() && member_b.up.is_some();
        let both_routers = member_a.is_router() && member_b.is_router();
        let shared_group =
            member_a.group.is_some() && member_a.group == member_b.group && !both_routers;
        // up first: only the host of a member that is up surely still exists
        both_up
            && ((shared_group && self.scouts(&member_a.host, &member_b.host))
                || (member_a.connects.contains(&b) && self.connect_dials(member_a, member_b))
                || (member_b.connects.contains(&a) && self.connect_dials(member_b, member_a)))
    }

    /// whether the connect of the member `from` to the member `to` gets through
    fn connect_dials(&self, from: &MemberState, to: &MemberState) -> bool {
        self.route_exists(&from.host, &to.host, get_listen_port(to).get())
    }

    /// the members joined to any of `start` by direct links, hop by hop, `start` included
    fn get_component(
        &self,
        state: &State,
        start: impl IntoIterator<Item = usize>,
    ) -> HashSet<usize> {
        let mut component: HashSet<usize> = start.into_iter().collect();
        let mut queue: Vec<usize> = component.iter().copied().collect();
        while let Some(index) = queue.pop() {
            for other in 0..state.members.len() {
                if !component.contains(&other) && self.is_direct_link(state, index, other) {
                    component.insert(other);
                    queue.push(other);
                }
            }
        }
        component
    }

    /// whether `member` is a member of this overlay
    pub(crate) fn has_member(&self, member: &Member) -> bool {
        Arc::ptr_eq(&member.overlay.shared, &self.shared)
    }

    /// whether `host` is in one of the host sets of the overlay's world
    pub(crate) fn world_has_host(&self, host: &host::Id) -> bool {
        self.shared.world.has_host(host)
    }

    /// Whether traffic from `from` to `port` on `to` gets through: the world has a route, and the
    /// firewalls of both hosts let the traffic through. Port 0 stands for whatever port the system
    /// on `to` picks.
    fn route_exists(&self, from: &host::Id, to: &host::Id, port: u16) -> bool {
        self.shared.world.route_exists(from, to, port) && host::firewalls_let_through(from, to)
    }

    /// whether what runs on the hosts `a` and `b` finds each other by scouting: multicast gets
    /// between the hosts, and their firewalls let it through
    fn scouts(&self, a: &host::Id, b: &host::Id) -> bool {
        self.shared.world.has_multicast_route(a, b) && host::firewalls_let_through(a, b)
    }

    /// whether gossip can link the hosts `a` and `b`: either may dial the other, on whatever port
    /// the other's system picked
    fn dial_either_way(&self, a: &host::Id, b: &host::Id) -> bool {
        self.route_exists(a, b, 0) || self.route_exists(b, a, 0)
    }

    /// How `dialler` dials `target`, the target of a connect, which has a listen port. The route
    /// exists even while a firewall drops the traffic on it, so the connect gets through once the
    /// firewall stops dropping it.
    fn get_endpoint(&self, dialler: &host::Id, target: &MemberState) -> EndPoint {
        self.shared
            .world
            .get_route(dialler, &target.host, get_listen_port(target))
            .unwrap_or_else(|| {
                panic!(
                    "{dialler:?} has no route to {:?} for a connect",
                    target.host
                )
            })
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

    /// a new member on the host `host`, scouting on `group` and connecting to `connects`
    fn add_member(&self, host: host::Id, group: Option<GroupId>, connects: Vec<usize>) -> Member {
        assert!(
            self.world_has_host(&host),
            "{host:?} is in none of the host sets of the overlay's world"
        );
        let mut state = self.lock();
        state.members.push(MemberState {
            host,
            group,
            connects,
            listen_port: None,
            up: None,
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

    /// how `dialler` dials the member for a connect, e.g. with the CLI's `--connect`; it must be
    /// a connect's target
    pub(crate) fn get_endpoint(&self, dialler: &host::Id) -> EndPoint {
        let index = self.overlay.index_of(self);
        self.overlay
            .get_endpoint(dialler, &self.overlay.lock().members[index])
    }

    /// whether the member is up on the network: from its start until its running handle goes away
    /// (see [`Self::come_up`])
    pub(crate) fn is_up(&self) -> bool {
        let index = self.overlay.index_of(self);
        self.overlay.lock().members[index].up.is_some()
    }

    /// Report that the member is now up in `mode`; a runtime's start calls this once the runtime is
    /// up. The member counts as up until the returned guard is dropped. The runtime's running
    /// handle holds the guard, so however the handle goes away, the overlay learns that the member
    /// went down.
    pub(crate) fn come_up(&self, mode: WhatAmI) -> UpGuard {
        let index = self.overlay.index_of(self);
        let mut state = self.overlay.lock();
        let member = &mut state.members[index];
        assert!(member.up.is_none(), "{self:?} was started while it is up");
        member.up = Some(mode);
        UpGuard {
            member: self.clone(),
        }
    }

    /// Give the member a listen port unless it already has one (from a connect, or set by the
    /// test): a port that is free on the test's machine right now. What connects to the member, and
    /// what the member listens on from its next start, use that port.
    pub(crate) fn ensure_listen_port(&self) {
        let index = self.overlay.index_of(self);
        let mut state = self.overlay.lock();
        let member = &mut state.members[index];
        if member.listen_port.is_none() {
            assert!(
                member.up.is_none(),
                "a member that is up got a listen port it doesn't listen on until its next start"
            );
            member.listen_port = Some(free_port());
        }
    }

    /// The `zenoh` section of this member's config, as of now: the group it scouts on (with
    /// multicast scouting off for a member attached by a connect), the members it connects to, and
    /// the port it listens on (any free port if no connect targets it).
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
            .map(|&target| {
                self.overlay
                    .get_endpoint(&member.host, &state.members[target])
                    .to_string()
            })
            .collect();
        // 0: any free port
        let listen_port = member.listen_port.map_or(0, NonZeroU16::get);
        serde_json::json!({
            "scouting": { "multicast": scouting },
            "connect": { "endpoints": connects },
            "listen": { "endpoints": [format!("tcp/[::]:{listen_port}")] },
        })
    }
}

/// What records a member as up in its overlay, for as long as it exists (see
/// [`Member::come_up`]).
pub(crate) struct UpGuard {
    member: Member,
}

impl Drop for UpGuard {
    fn drop(&mut self) {
        let index = self.member.overlay.index_of(&self.member);
        self.member.overlay.lock().members[index].up = None;
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

impl Attachment {
    /// the overlay this attaches to
    pub fn get_overlay(&self) -> &Overlay {
        match self {
            Self::Group(group) => group.get_overlay(),
            Self::ConnectTo(target) => target.get_overlay(),
        }
    }

    /// A zenoh config for a test's own session attached this way: on a group, a peer that
    /// scouts on it ([`MulticastGroup::get_zenoh_config`]); by a connect, a client that connects
    /// to the target only, whose open fails unless the target already runs. The session is no
    /// member of the overlay, so no swarm expects links to it.
    pub fn get_zenoh_config(&self) -> zenoh::Config {
        match self {
            Self::Group(group) => group.get_zenoh_config(),
            Self::ConnectTo(target) => {
                let mut config = zenoh::Config::default();
                config
                    .set_mode(Some(zenoh::config::WhatAmI::Client))
                    .expect("setting zenoh mode cannot fail");
                config
                    .insert_json5(
                        "connect/endpoints",
                        &serde_json::json!([target
                            .get_endpoint(&host::get_local_id())
                            .to_string()])
                        .to_string(),
                    )
                    .expect("a connect endpoint is valid");
                config
                    .insert_json5("scouting/multicast/enabled", "false")
                    .expect("disabling multicast scouting cannot fail");
                config
            }
        }
    }

    /// the `myrmic` arguments that have a CLI call on the host `host` find the runtimes this way
    pub(crate) fn get_cli_args(&self, host: &host::Id) -> [String; 2] {
        match self {
            Self::Group(group) => ["--multicast-group".to_owned(), group.get_address()],
            Self::ConnectTo(target) => [
                "--connect".to_owned(),
                target.get_endpoint(host).to_string(),
            ],
        }
    }

    /// Make a new member of the overlay on the host `host`, attached this way; what a
    /// runtime calls when it is created.
    pub(crate) fn add_member(&self, host: host::Id) -> Member {
        match self {
            Self::Group(group) => group.overlay.add_member(host, Some(group.id), vec![]),
            Self::ConnectTo(target) => {
                let overlay = target.get_overlay();
                overlay.add_member(host, None, vec![overlay.index_of(target)])
            }
        }
    }
}

impl From<&MulticastGroup> for Attachment {
    fn from(group: &MulticastGroup) -> Self {
        Self::Group(group.clone())
    }
}

/// the port `member` listens on, which a connect's target has from the connect on
fn get_listen_port(member: &MemberState) -> NonZeroU16 {
    member
        .listen_port
        .expect("a connect's target has a listen port from the connect on")
}

/// A TCP port that is free on the test's machine right now. Another process may take it before the
/// member listens on it; the member's start then fails.
fn free_port() -> NonZeroU16 {
    let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .and_then(|listener| listener.local_addr())
        .expect("failed to find a free port")
        .port();
    NonZeroU16::new(port).expect("the system picks no port 0")
}

/// A multicast group that no other overlay uses. The address comes from this process's pid (in the
/// administratively scoped 239.0.0.0/8, whose low 24 bits cover every Linux pid), and the port from
/// a counter per process. The counter starts above 7446, the port of sorg-tests' group per process
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
