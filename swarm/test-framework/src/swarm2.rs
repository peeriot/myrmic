//! A swarm of myrmic runtimes for tests: started from the runtimes a [`SwarmSpec`] names,
//! checked to be connected, and controlled per runtime.
//!
//! The name is temporary: this replaces [`crate::swarm`] and takes over its name once that is
//! gone.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Debug;
use std::hash::Hash;

use cell_protocol::{CellInstance, PlacementEntry, RuntimeId, Sri};
use zenoh::config::ZenohId;

pub use backend::local::Local;
pub use backend::{CutOff, SwarmBackend};

use crate::myrmic::{self, Error};

pub mod backend;
mod check;

/// The makeup of a swarm: the implementing type names its runtimes, one value per runtime, and
/// says how each one is started.
///
/// A test names its runtimes with an enum, one variant per runtime:
///
/// ```ignore
/// #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
/// enum Node { Leader, Follower }
///
/// impl SwarmSpec for Node {
///     fn all() -> Vec<Self> { vec![Node::Leader, Node::Follower] }
///     fn tags(self) -> Vec<String> {
///         match self {
///             Node::Leader => vec!["leader".to_owned()],
///             Node::Follower => vec![],
///         }
///     }
///     fn persistent(self) -> bool { false }
/// }
/// ```
///
/// A value's `Debug` output is its runtime's name, so distinct values must print distinctly.
pub trait SwarmSpec: Copy + Eq + Hash + Debug {
    /// every runtime of the swarm, each once
    fn all() -> Vec<Self>;

    /// the tags the runtime is started with
    fn tags(self) -> Vec<String>;

    /// whether the runtime keeps its database across restarts (no `--tmp`)
    fn persistent(self) -> bool;
}

/// A running swarm: the runtimes of `S` on a [`SwarmBackend`].
///
/// Dropping it deletes the runtimes that are still running (see [`myrmic::Runtime`]).
pub struct Swarm<B, S>
where
    B: SwarmBackend,
    S: SwarmSpec,
{
    // Declared before `backend`, so the runtimes are deleted while the backend's shims are still
    // there.
    runtimes: HashMap<S, Runtime<B::Cli>>,
    introspection: introspection_client::v1::Client,
    sorg: sorg_client::Client,
    session: zenoh::Session,
    backend: B,
}

/// One runtime of a [`Swarm`], see [`Swarm::runtime`].
pub struct Runtime<B>
where
    B: myrmic::MyrmicBackend,
{
    name: String,
    zid: ZenohId,
    /// `None` while the runtime is killed or deleted
    running: Option<myrmic::Runtime<B>>,
}

impl<B> Runtime<B>
where
    B: myrmic::MyrmicBackend,
{
    /// the runtime's name, its id's `Debug` output
    pub fn name(&self) -> &str {
        &self.name
    }

    /// the zenoh id the runtime runs under, the same across restarts
    pub fn zid(&self) -> ZenohId {
        self.zid
    }

    /// whether the runtime is running, i.e. not killed or deleted
    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }
}

impl<B, S> Swarm<B, S>
where
    B: SwarmBackend,
    S: SwarmSpec,
{
    /// Start every runtime of `S` on `backend`.
    ///
    /// Returns once the swarm is connected: each runtime answers the test's status query and is
    /// linked to all the other runtimes, and no other runtime answers.
    pub async fn start(backend: B) -> Result<Self, Error> {
        let ids = S::all();
        let started = futures::future::join_all(ids.iter().map(|&id| {
            let name = format!("{id:?}");
            let myrmic = backend.shim(&name);
            start_runtime(myrmic, name, id)
        }))
        .await;

        let mut runtimes = HashMap::new();
        for (id, runtime) in ids.into_iter().zip(started) {
            let runtime = runtime?;
            let swarm_runtime = Runtime {
                name: runtime.name().to_owned(),
                zid: runtime.zid(),
                running: Some(runtime),
            };
            let previous = runtimes.insert(id, swarm_runtime);
            assert!(previous.is_none(), "`all()` lists {id:?} twice");
        }
        let names: BTreeSet<&str> = runtimes.values().map(Runtime::name).collect();
        assert_eq!(
            names.len(),
            runtimes.len(),
            "two runtimes print the same name"
        );

        let session = backend.connect_session().await;
        let swarm = Self {
            runtimes,
            introspection: introspection_client::v1::Client::new(session.clone()).await,
            sorg: sorg_client::Client::new(session.clone()),
            session,
            backend,
        };
        let expected = swarm.runtimes.values().map(Runtime::zid).collect();
        wait_until_connected(&swarm.introspection, &expected).await?;
        Ok(swarm)
    }

    /// the runtime `id`
    pub fn runtime(&self, id: S) -> &Runtime<B::Cli> {
        self.runtimes.get(&id).expect("every runtime was started")
    }

    /// the runtime `id`, to kill, restart or delete it
    pub fn runtime_mut(&mut self, id: S) -> RuntimeMut<'_, B, S> {
        let others = self
            .runtimes
            .iter()
            .filter(|(other, runtime)| **other != id && runtime.is_running())
            .map(|(_, runtime)| runtime.zid)
            .collect();
        RuntimeMut {
            id,
            runtime: self
                .runtimes
                .get_mut(&id)
                .expect("every runtime was started"),
            backend: &self.backend,
            introspection: &self.introspection,
            others,
        }
    }

    /// The runtime the cell `sri` is placed on, or `None` while it has no placement or its
    /// placement names no host yet.
    ///
    /// A killed runtime keeps its placements until the swarm has declared it dead, so during a
    /// failover this still names the killed runtime for a while.
    pub async fn host_of(&self, sri: &str) -> Option<S> {
        let host = self.placement(sri).await?.kind.host()?;
        let (id, _) = self
            .runtimes
            .iter()
            .find(|(_, runtime)| RuntimeId::from(runtime.zid) == host)
            .unwrap_or_else(|| panic!("{sri} is placed on {host}, which is not in the swarm"));
        Some(*id)
    }

    /// The placement row of the cell `sri`, or `None` while it has none.
    pub async fn placement(&self, sri: &str) -> Option<PlacementEntry> {
        let sri: Sri = sri.parse().expect("a deployed cell's SRI is valid");
        self.sorg
            .get_placement(&sri)
            .await
            .expect("failed to query the cell's placement")
    }

    /// The instance row of the cell `sri`, or `None` while it has none.
    pub async fn instance(&self, sri: &str) -> Option<CellInstance> {
        let sri: Sri = sri.parse().expect("a deployed cell's SRI is valid");
        sorg_common::instance_registry::get_instance(&self.session, &sri)
            .await
            .expect("failed to query the cell's instance")
    }

    /// a zenoh session that is part of the swarm, e.g. for [`crate::clients::db::DbHandle`]
    pub fn session(&self) -> &zenoh::Session {
        &self.session
    }

    /// The CLI for the commands that act on the whole swarm, e.g. [`myrmic::Myrmic::cell`] to
    /// deploy a cell (see [`SwarmBackend::client`]).
    pub fn client(&self) -> &myrmic::Myrmic<B::Client> {
        self.backend.client()
    }
}

/// Wait until exactly the runtimes `expected` answer the status query on `introspection`, each
/// linked to all the others. Fails with the last mismatch after [`crate::wait::DEFAULT_TIMEOUT`].
async fn wait_until_connected(
    introspection: &introspection_client::v1::Client,
    expected: &BTreeSet<ZenohId>,
) -> Result<(), Error> {
    let mut last_mismatch = String::new();
    let polled = tokio::time::timeout(crate::wait::DEFAULT_TIMEOUT, async {
        loop {
            let statuses = introspection
                .swarm_status()
                .await
                .expect("failed to query the swarm's status");
            match check::mismatch(&statuses, expected) {
                None => return,
                Some(mismatch) => last_mismatch = mismatch,
            }
            tokio::time::sleep(crate::wait::DEFAULT_POLL_INTERVAL).await;
        }
    })
    .await;
    polled.map_err(|_| Error::Timeout {
        waited_for: format!("the swarm to be connected; last: {last_mismatch}"),
    })
}

/// `myrmic runtimes start` of the runtime `id`, named `name`.
async fn start_runtime<B, S>(
    myrmic: &myrmic::Myrmic<B>,
    name: String,
    id: S,
) -> Result<myrmic::Runtime<B>, Error>
where
    B: myrmic::MyrmicBackend,
    S: SwarmSpec,
{
    let tags = id.tags();
    let tags: Vec<&str> = tags.iter().map(String::as_str).collect();
    myrmic
        .runtime(&tags)
        .name(&name)
        .persistent(id.persistent())
        .start()
        .await
}

/// One runtime of a [`Swarm`] with its lifecycle, see [`Swarm::runtime_mut`].
///
/// It borrows the swarm: a restart waits for the swarm to be connected again, so it needs the
/// other runtimes to stay as they are.
pub struct RuntimeMut<'a, B, S>
where
    B: SwarmBackend,
    S: SwarmSpec,
{
    id: S,
    runtime: &'a mut Runtime<B::Cli>,
    backend: &'a B,
    introspection: &'a introspection_client::v1::Client,
    /// the zids of the swarm's other running runtimes
    others: BTreeSet<ZenohId>,
}

impl<B, S> RuntimeMut<'_, B, S>
where
    B: SwarmBackend,
    S: SwarmSpec,
{
    /// End the runtime abruptly (see [`myrmic::Runtime::kill`]). Returns once its process is
    /// gone; it does not wait for the rest of the swarm to notice.
    pub async fn kill(&mut self) -> Result<(), Error> {
        self.take_running().kill().await
    }

    /// Shut the runtime down gracefully (see [`myrmic::Runtime::delete`]).
    pub async fn delete(&mut self) -> Result<(), Error> {
        self.take_running().delete().await
    }

    /// Start the killed or deleted runtime again under its name, so under the same zid. Returns
    /// once the swarm, with this runtime, is connected again.
    pub async fn restart(&mut self) -> Result<(), Error> {
        let shim = self.backend.shim(&self.runtime.name);
        self.start_on(shim).await?;
        let mut expected = self.others.clone();
        expected.insert(self.runtime.zid);
        wait_until_connected(self.introspection, &expected).await
    }

    /// Start the killed or deleted runtime under its name on `shim`.
    async fn start_on(&mut self, shim: &myrmic::Myrmic<B::Cli>) -> Result<(), Error> {
        let id = self.id;
        assert!(!self.runtime.is_running(), "{id:?} is still running");
        let started = start_runtime(shim, self.runtime.name.clone(), id).await?;
        assert_eq!(
            started.zid(),
            self.runtime.zid,
            "{id:?} came back under a different zid"
        );
        self.runtime.running = Some(started);
        Ok(())
    }

    /// the running runtime, now marked as no longer running
    fn take_running(&mut self) -> myrmic::Runtime<B::Cli> {
        let id = self.id;
        self.runtime
            .running
            .take()
            .unwrap_or_else(|| panic!("{id:?} is not running"))
    }
}

impl<B, S> RuntimeMut<'_, B, S>
where
    B: CutOff,
    S: SwarmSpec,
{
    /// Start the killed or deleted runtime again under its name, so with its zid and database,
    /// but cut off from the rest of the swarm (see [`CutOff`]). Returns once it is ready; no
    /// connectivity check, it reaches no other runtime. [`Self::delete`] or [`Self::kill`] it
    /// before a [`Self::restart`] brings it back into the swarm.
    pub async fn start_cut_off(&mut self) -> Result<(), Error> {
        let shim = self.backend.cut_off_shim(&self.runtime.name);
        self.start_on(&shim).await
    }
}
