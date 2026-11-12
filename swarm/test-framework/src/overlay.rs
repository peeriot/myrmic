//! The zenoh links a test's runtimes form among themselves: which runtimes scout on which
//! multicast group, and so link to each other. An [`Overlay`] is the one place that knows this;
//! runtimes read their zenoh settings from it when they start, and a swarm reads from it which
//! links it expects.
//!
//! # Using it
//!
//! ```ignore
//! use test_framework::{in_process, myrmic::Myrmic, overlay::Overlay};
//!
//! let group = Overlay::new().new_multicast_group();
//! let myrmic = Myrmic::local_isolated(&group);
//! let a = myrmic.runtime();  // a CLI runtime that scouts on `group`
//! let b = in_process::StoppedRt::new(&group).orchestration();  // a node on the same group
//! let session = zenoh::open(group.get_zenoh_config()).await.unwrap();  // the test, on the group too
//! ```
//!
//! - A runtime becomes a member of the overlay when it is created, on the group it is created
//!   with. Every member of a group links to every other member of it.
//! - [`Overlay::new_multicast_group`] hands out a group no other overlay uses, so runtimes of
//!   different overlays (e.g. of tests running in parallel) never see each other.
//! - A group holds its overlay, so a test that only needs the group need not keep the overlay.
//!
//! # Design notes
//!
//! The overlay is the zenoh view of a test's setup, as opposed to the network the hosts sit on:
//! a link between two runtimes exists if the overlay wants it and the network lets their hosts
//! reach each other. Runtimes don't keep their group themselves: they keep their membership, and
//! ask the overlay for their settings at start, so the overlay is the only source of who links
//! to whom.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU16, AtomicU64, Ordering},
};

/// The zenoh links among a test's runtimes; see [the module docs](self). Clones are the same
/// overlay.
#[derive(Clone)]
pub struct Overlay {
    shared: Arc<Shared>,
}

/// What the clones of an overlay and its groups share.
struct Shared {
    /// tells this overlay's members apart from other overlays'
    id: u64,
    /// the multicast group (`<address>:<port>`) of each [`GroupId`], at its index
    group_addresses: Mutex<Vec<String>>,
    /// the group each member scouts on, at the member's index
    member_groups: Mutex<Vec<GroupId>>,
}

/// One of an overlay's multicast groups, by its index in [`Shared::group_addresses`].
#[derive(Clone, Copy, PartialEq, Eq)]
struct GroupId(usize);

/// A multicast group of an [`Overlay`]: the runtimes created on it scout on it, and so link to
/// each other. Clones are the same group.
#[derive(Clone)]
pub struct MulticastGroup {
    overlay: Overlay,
    id: GroupId,
}

/// Which member of which overlay a runtime is (see [`crate::runtime::Stopped::get_member_id`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct MemberId {
    overlay: u64,
    index: usize,
}

impl Overlay {
    /// an overlay with no group and no member yet
    pub fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        Self {
            shared: Arc::new(Shared {
                id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
                group_addresses: Mutex::new(vec![]),
                member_groups: Mutex::new(vec![]),
            }),
        }
    }

    /// a new multicast group of this overlay, which no other overlay uses
    pub fn new_multicast_group(&self) -> MulticastGroup {
        let mut addresses = self.shared.group_addresses.lock().expect("not poisoned");
        addresses.push(isolated_multicast_group());
        MulticastGroup {
            overlay: self.clone(),
            id: GroupId(addresses.len() - 1),
        }
    }

    /// Whether the members `a` and `b`, both of this overlay, link to each other: whether they
    /// scout on the same group.
    pub(crate) fn are_linked(&self, a: MemberId, b: MemberId) -> bool {
        let groups = self.shared.member_groups.lock().expect("not poisoned");
        groups[self.index_of(a)] == groups[self.index_of(b)]
    }

    /// whether `member` is a member of this overlay
    pub(crate) fn has_member(&self, member: MemberId) -> bool {
        member.overlay == self.shared.id
    }

    /// the group the member `member` scouts on, which its zenoh config names when it starts
    pub(crate) fn get_group_of(&self, member: MemberId) -> MulticastGroup {
        let id = self.shared.member_groups.lock().expect("not poisoned")[self.index_of(member)];
        MulticastGroup {
            overlay: self.clone(),
            id,
        }
    }

    /// `member`'s index among this overlay's members
    fn index_of(&self, member: MemberId) -> usize {
        assert!(
            self.has_member(member),
            "{member:?} is not a member of this overlay"
        );
        member.index
    }
}

impl Default for Overlay {
    fn default() -> Self {
        Self::new()
    }
}

impl MulticastGroup {
    /// the overlay this group belongs to
    pub fn get_overlay(&self) -> &Overlay {
        &self.overlay
    }

    /// Make a new member of the overlay that scouts on this group; what a runtime calls when it
    /// is created on the group.
    pub(crate) fn add_member(&self) -> MemberId {
        let mut groups = self
            .overlay
            .shared
            .member_groups
            .lock()
            .expect("not poisoned");
        groups.push(self.id);
        MemberId {
            overlay: self.overlay.shared.id,
            index: groups.len() - 1,
        }
    }

    /// the group's address (`<address>:<port>`), e.g. for the CLI's `--multicast-group`
    pub fn get_address(&self) -> String {
        let GroupId(group) = self.id;
        self.overlay
            .shared
            .group_addresses
            .lock()
            .expect("not poisoned")[group]
            .clone()
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
