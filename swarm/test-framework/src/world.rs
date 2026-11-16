//! The world a test's hosts live in ([`World`]) and its sets of hosts ([`host_set`]), re-exported
//! from the crate root.

use std::{
    num::NonZeroU16,
    sync::{Arc, Mutex, MutexGuard},
};

use zenoh::config::EndPoint;

use self::host_set::ConnectedHostSet;
use crate::host;

pub mod host_set;

/// The world a test's hosts live in: the sets of hosts that can dial each other ([`host_set`]),
/// all of them together. An [`crate::overlay::Overlay`] sits on a world and asks it whether one
/// host can dial another, and how.
///
/// ```ignore
/// use test_framework::{World, docker, host_set, overlay::Overlay, world};
///
/// let overlay = Overlay::new(&World::local());  // only the test's own machine
///
/// let docker = host_set::Docker::new(&docker::Daemon::connect()).await;
/// let world = world![docker];  // the docker network, with the test's machine on it
/// let overlay = Overlay::new(&world);
/// ```
///
/// A host may be in several host sets, so a route between two hosts may go through any set both
/// are in, and a world answers by asking those. It answers nothing beyond that: routes go one hop
/// (see [`host_set`]), so two hosts that share no set have no route, whatever sets lie between
/// them.
///
/// A world is shared: clones are the same world, and a host set added to it later (e.g. a tunnel
/// that needs a port the overlay hands out first) is part of the answers from then on.
#[derive(Clone, Default)]
pub struct World {
    host_sets: Arc<Mutex<Vec<Arc<dyn ConnectedHostSet>>>>,
}

/// A [`World`] of the host sets `a`, `b`, ...: [`World::new`], then [`World::add`] for each.
#[macro_export]
macro_rules! world {
    ($($host_set:expr),* $(,)?) => {{
        let world = $crate::World::new();
        $(world.add(&$host_set);)*
        world
    }};
}

impl World {
    /// a world with no host set yet
    pub fn new() -> Self {
        Self::default()
    }

    /// a world of the test's own machine alone ([`host_set::Local`]), for a test whose runtimes
    /// all run on it
    pub fn local() -> Self {
        let world = Self::new();
        world.add(&host_set::Local);
        world
    }

    /// add `host_set`: from now on, routes may go through it
    pub fn add<S: ConnectedHostSet + Clone + 'static>(&self, host_set: &S) {
        self.lock().push(Arc::new(host_set.clone()));
    }

    /// whether `host` is in one of the world's host sets
    pub(crate) fn has_host(&self, host: &host::Id) -> bool {
        self.lock().iter().any(|host_set| host_set.has_host(host))
    }

    /// whether `from` can dial `port` on `to` through one of the host sets both are in; port 0
    /// stands for whatever port the system on `to` picks
    pub(crate) fn route_exists(&self, from: &host::Id, to: &host::Id, port: u16) -> bool {
        self.get_common_host_sets(from, to)
            .iter()
            .any(|host_set| host_set.route_exists(from, to, port))
    }

    /// how `from` dials `port` on `to`, through the first host set both are in that has a route
    pub(crate) fn get_route(
        &self,
        from: &host::Id,
        to: &host::Id,
        port: NonZeroU16,
    ) -> Option<EndPoint> {
        self.get_common_host_sets(from, to)
            .iter()
            .find_map(|host_set| host_set.get_route(from, to, port))
    }

    /// whether multicast gets between `a` and `b`, so what runs on them finds each other by
    /// scouting: a host set both are in carries it, and one of them can dial the other through it
    pub(crate) fn has_multicast_route(&self, a: &host::Id, b: &host::Id) -> bool {
        self.get_common_host_sets(a, b).iter().any(|host_set| {
            host_set.has_multicast()
                && (host_set.route_exists(a, b, 0) || host_set.route_exists(b, a, 0))
        })
    }

    /// the host sets both `a` and `b` are in
    fn get_common_host_sets(&self, a: &host::Id, b: &host::Id) -> Vec<Arc<dyn ConnectedHostSet>> {
        self.lock()
            .iter()
            .filter(|host_set| host_set.has_host(a) && host_set.has_host(b))
            .cloned()
            .collect()
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Arc<dyn ConnectedHostSet>>> {
        self.host_sets.lock().expect("not poisoned")
    }
}
