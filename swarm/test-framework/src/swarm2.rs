//! A swarm of myrmic runtimes for tests: started from the runtimes a [`SwarmSpec`] names,
//! checked to be connected, and operated as a whole.
//!
//! # Using it
//!
//! ```ignore
//! use test_framework::runtime::{Kill, Running, Stopped};
//! use test_framework::swarm2::{IntoRuntimeMap, Local, Swarm};
//!
//! let (swarm, [a, b, c]) = Swarm::<_, Node>::start(Local::new()).await?;
//! let b = b.kill().await?; // stopped
//! let b = b.start().await?; // running again; returns once the swarm is connected again
//! ```
//!
//! - [`Swarm::start`] starts every runtime and hands the test one running handle per runtime, in
//!   the order of the spec's variants, so an array pattern names them. [`Swarm::new`] hands out
//!   stopped handles instead, for tests that start runtimes one after another
//!   ([`crate::start_runtimes!`] starts several side by side).
//! - The handles ([`StoppedRt`], [`RunningRt`]) move through their states like any
//!   [`crate::runtime`] handle. Starting one returns once the swarm, with it, is connected again.
//! - A test that picks runtimes by an id it computes (e.g. the host of a cell, from
//!   [`Swarm::host_of`]) turns the array into a map ([`IntoRuntimeMap`]): an `EnumMap` to borrow
//!   a handle by id, a `HashMap` to take one out for a transition that consumes it.
//! - Faults that act on a runtime's host ([`Swarm::isolate`], [`Swarm::skew_clock`], ...) take the
//!   runtime's id and need no handle: the host is there whether the runtime runs or not.
//! - [`Swarm::client`] is the CLI that operates the swarm as a whole (deploys, sends, status), and
//!   [`Swarm::session`] / [`Swarm::sorg`] reach its services directly.
//!
//! # Design notes
//!
//! The swarm does not own its runtimes: the test does, so that a runtime's state is its handle's
//! type. The swarm keeps one entry per runtime ([`RuntimeEntry`]) with what does not change with
//! the state (name, zid) and whether the test has the runtime running. That flag is the test's
//! intent, set by a start and cleared when the running handle goes away; the connectivity check
//! compares it with the runtimes that actually answer, which is what catches a runtime that died
//! without the test stopping it. How the handles keep that flag is in the handles' module
//! (`rt`, re-exported here).
//!
//! The name is temporary: this replaces [`crate::swarm`] and takes over its name once that is
//! gone.

use std::{
    collections::BTreeSet,
    fmt::Debug,
    future::Future,
    marker::PhantomData,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use cell_protocol::{CellInstance, PlacementEntry, RuntimeId, Sri};
use db_client::v1::models::{Scope, Subject};
use enum_map::{Array, Enum, EnumMap};
use zenoh::config::ZenohId;

pub use backend::{
    Block, CutOff, FillDisk, Isolate, Readdress, SkewClock, SwarmBackend, docker::Docker,
    local::Local, ssh::Ssh,
};
pub use rt::{IntoRuntimeMap, RunningRt, StoppedRt, SwarmRt, start_together};

use crate::{
    myrmic::{self, Error},
    runtime::Stopped,
};

pub mod backend;
mod check;
mod rt;

/// The makeup of a swarm: the implementing type names its runtimes, one value per runtime, and
/// says how each one is started.
///
/// A test names its runtimes with an enum, one variant per runtime; deriving [`Enum`] makes its
/// variants the swarm's runtimes, each once:
///
/// ```ignore
/// #[derive(Clone, Copy, PartialEq, Eq, Debug, enum_map::Enum)]
/// enum Node { Leader, Follower }
///
/// impl SwarmSpec for Node {
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
/// A value's `Debug` output is its runtime's name, so distinct values must print distinctly. It
/// is `Send + Sync` because the handles' `Send` futures hold it across their awaits.
pub trait SwarmSpec: Enum + Copy + Eq + Debug + Send + Sync {
    /// the tags the runtime is started with
    fn tags(self) -> Vec<String>;

    /// whether the runtime keeps its database across restarts (no `--tmp`)
    fn persistent(self) -> bool;
}

/// What a failed transition of a swarm runtime on `B` reports: the backend's runtime error, which
/// also has to carry the connectivity check's [`Error::Timeout`].
pub type RtError<B, S> = <<B as SwarmBackend<S>>::Runtime as Stopped>::Error;

/// A swarm: the runtimes of `S` on a [`SwarmBackend`], and what the test operates it with. See
/// [the module docs](self).
pub struct Swarm<B, S> {
    shared: Arc<Shared<B, S>>,
    sorg: sorg_client::Client,
    session: zenoh::Session,
}

/// What a swarm shares with its runtimes' handles.
struct Shared<B, S> {
    backend: B,
    /// One per runtime, at its [`Enum::into_usize`] index. A slice rather than an [`EnumMap`]:
    /// whether an `EnumMap` is `Sync` depends on `S::Array`, which the compiler cannot see through
    /// in generic code, and the handles' futures need this to be `Sync`.
    entries: Box<[RuntimeEntry]>,
    introspection: introspection_client::v1::Client,
    spec: PhantomData<S>,
}

/// What a [`Swarm`] knows about one of its runtimes, whatever state it is in (see
/// [`Swarm::get_entry`]).
pub struct RuntimeEntry {
    name: String,
    /// set at the runtime's first start, the same on every later one
    zid: OnceLock<ZenohId>,
    /// whether the test has the runtime running: set by a start, cleared when the running handle
    /// goes away
    running: AtomicBool,
}

impl RuntimeEntry {
    /// the runtime's name, its id's `Debug` output
    pub fn get_name(&self) -> &str {
        &self.name
    }

    /// The zenoh id the runtime runs under, the same across restarts. Panics for a runtime that
    /// was never started: it has no zid yet.
    pub fn get_zid(&self) -> ZenohId {
        *self
            .zid
            .get()
            .unwrap_or_else(|| panic!("`{}` was never started, so it has no zid", self.name))
    }

    /// whether the test has the runtime running, i.e. holds its running handle
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

impl<B, S> Swarm<B, S>
where
    B: SwarmBackend<S>,
    S: SwarmSpec,
{
    /// The swarm of `S` on `backend` with no runtime started: one stopped handle per runtime, in
    /// the order of the spec's variants. For tests that start runtimes one after another; the
    /// others use [`Self::start`].
    pub async fn new(backend: B) -> (Self, S::Array<StoppedRt<B, S>>) {
        let swarm = Self::create(backend).await;
        let stopped = EnumMap::<S, _>::from_fn(|id| StoppedRt {
            id,
            shared: swarm.shared.clone(),
        })
        .into_array();
        (swarm, stopped)
    }

    /// The swarm with every runtime of `S` started on `backend`: one running handle per runtime,
    /// in the order of the spec's variants.
    ///
    /// Returns once the swarm is connected: each runtime answers the test's status query and is
    /// linked to all the other runtimes, and no other runtime answers.
    pub async fn start(backend: B) -> Result<(Self, S::Array<RunningRt<B, S>>), RtError<B, S>> {
        let swarm = Self::create(backend).await;
        let stopped = (0..S::Array::<()>::LENGTH).map(|i| StoppedRt {
            id: S::from_usize(i),
            shared: swarm.shared.clone(),
        });
        // in the order of `stopped`, i.e. of the variants, which `from_fn` visits in turn
        let mut running = rt::start_together(stopped).await?.into_iter();
        let running = EnumMap::<S, _>::from_fn(|_| {
            running
                .next()
                .expect("one running handle per started runtime")
        });
        Ok((swarm, running.into_array()))
    }

    /// the swarm, with every runtime stopped
    async fn create(backend: B) -> Self {
        let names: Vec<String> = (0..S::Array::<()>::LENGTH)
            .map(|i| format!("{:?}", S::from_usize(i)))
            .collect();
        // Checked before anything starts: two runtimes under one name would race in the CLI.
        let distinct_names: BTreeSet<&str> = names.iter().map(String::as_str).collect();
        assert_eq!(
            distinct_names.len(),
            names.len(),
            "two runtimes print the same name: {names:?}"
        );
        let entries = names
            .into_iter()
            .map(|name| RuntimeEntry {
                name,
                zid: OnceLock::new(),
                running: AtomicBool::new(false),
            })
            .collect();
        let session = backend.connect_session().await;
        Self {
            shared: Arc::new(Shared {
                backend,
                entries,
                introspection: introspection_client::v1::Client::new(session.clone()).await,
                spec: PhantomData,
            }),
            sorg: sorg_client::Client::new(session.clone()),
            session,
        }
    }

    /// what the swarm knows about the runtime `id`
    pub fn get_entry(&self, id: S) -> &RuntimeEntry {
        self.shared.entry(id)
    }

    /// every runtime's entry with its id, in the order of the spec's variants
    pub fn get_entries(&self) -> impl Iterator<Item = (S, &RuntimeEntry)> {
        self.shared.entries()
    }

    /// Wait until exactly the runtimes the test has running answer the status query, each linked
    /// to all the others. Starting a runtime already does this; a test calls it after something
    /// else that disconnects and reconnects runtimes.
    pub async fn wait_until_connected(&self) -> Result<(), Error> {
        self.shared.wait_until_connected().await
    }

    /// The runtime the cell `sri` is placed on, or `None` while it has no placement or its
    /// placement names no host yet. Fails like [`Self::placement`].
    ///
    /// A killed runtime keeps its placements until the swarm has declared it dead, so during a
    /// failover this still names the killed runtime for a while.
    pub async fn host_of(&self, sri: &str) -> Result<Option<S>, sorg_client::Error> {
        let host = self
            .placement(sri)
            .await?
            .and_then(|placement| placement.kind.host());
        Ok(host.map(|host| {
            let (id, _) = self
                .shared
                .started_entries()
                .find(|(_, zid)| RuntimeId::from(*zid) == host)
                .unwrap_or_else(|| panic!("{sri} is placed on {host}, which is not in the swarm"));
            id
        }))
    }

    /// The runtime holding the db scope `scope`; for a cell's data (`scope_of_cell`) it need not
    /// be the cell's host. Call it only while no replication entry covers the scope: until then
    /// it has exactly one holder.
    pub async fn scope_holder(&self, scope: &Scope) -> S {
        let holders =
            db_client::replica_v1::Client::new(&self.session, &Subject::Scope(scope.clone()))
                .expect("failed to open a db replica client")
                .locate(scope, None)
                .await
                .expect("failed to locate the scope");
        let [holder] = holders.as_slice() else {
            panic!("scope {scope:?} has {} holders, not one", holders.len());
        };
        let (id, _) = self
            .shared
            .started_entries()
            .find(|(_, zid)| zid.to_le_bytes() == holder.id)
            .unwrap_or_else(|| panic!("scope {scope:?} is held by a runtime not in the swarm"));
        id
    }

    /// The placement row of the cell `sri`, or `None` while it has none.
    ///
    /// Fails while the runtime holding the swarm's placement table is unreachable, until the
    /// swarm has moved the table (up to a minute after a runtime is lost, see the operating
    /// guide). A test that reads it while runtimes are lost or cut off retries.
    pub async fn placement(&self, sri: &str) -> Result<Option<PlacementEntry>, sorg_client::Error> {
        let sri: Sri = sri.parse().expect("a deployed cell's SRI is valid");
        self.sorg.get_placement(&sri).await
    }

    /// Every placement row, one per placed cell. Fails like [`Self::placement`].
    pub async fn placements(&self) -> Result<Vec<PlacementEntry>, sorg_client::Error> {
        self.sorg.list_placements().await
    }

    /// The instance row of the cell `sri`, or `None` while it has none. Fails like
    /// [`Self::placement`], for the instance table.
    pub async fn instance(&self, sri: &str) -> Result<Option<CellInstance>, sorg_client::Error> {
        let sri: Sri = sri.parse().expect("a deployed cell's SRI is valid");
        Ok(sorg_common::instance_registry::get_instance(&self.session, &sri).await?)
    }

    /// Why the swarm is not connected right now, or `None` if it is: exactly the running runtimes
    /// answer the test's status query, each linked to all the others. One query, no waiting, so a
    /// test can check that the swarm stays connected.
    pub async fn connection_mismatch(&self) -> Option<String> {
        let statuses = self
            .shared
            .introspection
            .swarm_status()
            .await
            .expect("failed to query the swarm's status");
        check::mismatch(&statuses, &self.shared.running_zids())
    }

    /// a zenoh session that is part of the swarm, e.g. for [`crate::clients::db::DbHandle`]
    pub fn session(&self) -> &zenoh::Session {
        &self.session
    }

    /// a sorg client on [`Self::session`], e.g. to deploy several cells in one request
    pub fn sorg(&self) -> &sorg_client::Client {
        &self.sorg
    }

    /// The CLI for the commands that act on the whole swarm, e.g. [`myrmic::Myrmic::cell`] to
    /// deploy a cell (see [`SwarmBackend::client`]).
    pub fn client(&self) -> myrmic::Myrmic<B::Client> {
        self.shared.backend.client()
    }
}

impl<B, S> Shared<B, S>
where
    S: Enum,
{
    fn entry(&self, id: S) -> &RuntimeEntry {
        &self.entries[id.into_usize()]
    }
}

impl<B, S> Shared<B, S>
where
    B: SwarmBackend<S>,
    S: SwarmSpec,
{
    fn entries(&self) -> impl Iterator<Item = (S, &RuntimeEntry)> {
        self.entries
            .iter()
            .enumerate()
            .map(|(i, entry)| (S::from_usize(i), entry))
    }

    /// the runtimes that were started at least once, with their zids
    fn started_entries(&self) -> impl Iterator<Item = (S, ZenohId)> {
        self.entries()
            .filter_map(|(id, entry)| entry.zid.get().map(|zid| (id, *zid)))
    }

    /// the zids of the runtimes the test has running
    fn running_zids(&self) -> BTreeSet<ZenohId> {
        self.entries()
            .filter(|(_, entry)| entry.is_running())
            .map(|(_, entry)| entry.get_zid())
            .collect()
    }

    async fn wait_until_connected(&self) -> Result<(), Error> {
        wait_until_connected(&self.introspection, &self.running_zids()).await
    }
}

impl<B, S> Swarm<B, S>
where
    B: Block<S>,
    S: SwarmSpec,
{
    /// Cut the link between the runtimes `a` and `b` (see [`Block::block`]). Both stay connected
    /// to everything else, and may still reach each other through it.
    pub async fn block(&self, a: S, b: S) {
        self.shared.backend.block(a, b).await;
    }

    /// Cut the link between the runtime `id` and the test's router (see
    /// [`Block::block_router`]).
    pub async fn block_router(&self, id: S) {
        self.shared.backend.block_router(id).await;
    }
}

impl<B, S> Swarm<B, S>
where
    B: Isolate<S>,
    S: SwarmSpec,
{
    /// Cut the runtime `id`'s host off the network (see [`Isolate::isolate`]). A running runtime
    /// keeps running, and the rest of the swarm only notices its silence.
    pub async fn isolate(&self, id: S) {
        self.shared.backend.isolate(id).await;
    }

    /// Connect the runtime `id`'s host again. Returns once the swarm is connected again.
    pub async fn heal(&self, id: S) -> Result<(), Error> {
        self.shared.backend.heal(id).await;
        self.wait_until_connected().await
    }

    /// Cut every running runtime off the network at once (see [`Self::isolate`]): each keeps
    /// running, alone.
    pub async fn isolate_all(&self) {
        for (id, entry) in self.shared.entries() {
            if entry.is_running() {
                self.shared.backend.isolate(id).await;
            }
        }
    }

    /// Connect every running runtime again, each of which must be isolated (see
    /// [`Self::isolate_all`]). Returns once the swarm is connected again.
    ///
    /// For runtimes isolated together: [`Self::heal`] of one of them waits for it to reach the
    /// others, which can't happen while they are still isolated.
    pub async fn heal_all(&self) -> Result<(), Error> {
        for (id, entry) in self.shared.entries() {
            if entry.is_running() {
                self.shared.backend.heal(id).await;
            }
        }
        self.wait_until_connected().await
    }
}

impl<B, S> Swarm<B, S>
where
    B: Readdress<S>,
    S: SwarmSpec,
{
    /// Move the runtime `id`'s host to another address (see [`Readdress::readdress`]). Returns at
    /// once: finding back into the swarm is the runtime's own business.
    pub async fn readdress(&self, id: S) {
        self.shared.backend.readdress(id).await;
    }
}

impl<B, S> Swarm<B, S>
where
    B: FillDisk<S>,
    S: SwarmSpec,
{
    /// Fill the disk the runtime `id` keeps its data on up to `room` free bytes (see
    /// [`FillDisk::fill_disk`]). A running runtime keeps running, and its writes start failing
    /// once the room is used up.
    pub async fn fill_disk(&self, id: S, room: u64) {
        self.shared.backend.fill_disk(id, room).await;
    }

    /// Free the disk filled by [`Self::fill_disk`] again.
    pub async fn free_disk(&self, id: S) {
        self.shared.backend.free_disk(id).await;
    }
}

impl<B, S> Swarm<B, S>
where
    B: SkewClock<S>,
    S: SwarmSpec,
{
    /// Set the runtime `id`'s wall clock to `ahead_secs` seconds ahead of the others' (behind them
    /// if negative; 0 puts it back in step), as a clock step on its host would (see
    /// [`SkewClock::skew_clock`]). A running runtime keeps running.
    pub async fn skew_clock(&self, id: S, ahead_secs: i64) {
        self.shared.backend.skew_clock(id, ahead_secs).await;
    }
}

/// Wait until exactly the runtimes `expected` answer the status query on `introspection`, each
/// linked to all the others, then [`LEADER_SETTLE_TIME`]. Fails with the last mismatch after
/// [`crate::wait::DEFAULT_TIMEOUT`].
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
    })?;
    tokio::time::sleep(LEADER_SETTLE_TIME).await;
    Ok(())
}

/// How long the orchestrators get to agree on a leader once the swarm is connected. Each one
/// learns of the others from join events, and until it has, one that knows no lower zid takes
/// itself for the leader: two leaders both handle a deploy, and the second answers "already
/// deployed". Nothing reports who considers itself leader, so this is a fixed wait.
const LEADER_SETTLE_TIME: Duration = Duration::from_secs(1);

/// `f` of every value of `S`, awaited concurrently: [`EnumMap::from_fn`] for an async `f`.
async fn join_enum_map<S, F>(f: impl Fn(S) -> F) -> EnumMap<S, F::Output>
where
    S: Enum,
    F: Future,
{
    let outputs =
        futures::future::join_all((0..S::Array::<()>::LENGTH).map(|i| f(S::from_usize(i)))).await;
    // `join_all` keeps the order of its futures, so output `i` is variant `i`'s
    let mut outputs: Vec<_> = outputs.into_iter().map(Some).collect();
    EnumMap::from_fn(|id: S| {
        outputs[id.into_usize()]
            .take()
            .expect("one output per variant")
    })
}
