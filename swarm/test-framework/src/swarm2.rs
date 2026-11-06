//! A swarm of myrmic runtimes for tests: started from the runtimes a [`SwarmSpec`] names,
//! checked to be connected, and controlled per runtime.
//!
//! The name is temporary: this replaces [`crate::swarm`] and takes over its name once that is
//! gone.

use std::collections::BTreeSet;
use std::fmt::Debug;
use std::future::Future;
use std::time::Duration;

use cell_protocol::{CellInstance, PlacementEntry, RuntimeId, Sri};
use db_client::v1::models::{Scope, Subject};
use enum_map::{Array, Enum, EnumMap};
use zenoh::config::ZenohId;

pub use backend::docker::Docker;
pub use backend::local::Local;
pub use backend::ssh::Ssh;
pub use backend::{Block, CutOff, FillDisk, Isolate, Readdress, SkewClock, SwarmBackend};

use crate::myrmic::{self, Error};

pub mod backend;
mod check;

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
/// is `Send` because the backends' `Send` futures hold it across their awaits.
pub trait SwarmSpec: Enum + Copy + Eq + Debug + Send {
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
    B: SwarmBackend<S>,
    S: SwarmSpec,
{
    // Declared before `backend`, so the runtimes are deleted while the backend's shims are still
    // there.
    runtimes: EnumMap<S, Runtime<B::Cli>>,
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
    B: SwarmBackend<S>,
    S: SwarmSpec,
{
    /// Start every runtime of `S` on `backend`.
    ///
    /// Returns once the swarm is connected: each runtime answers the test's status query and is
    /// linked to all the other runtimes, and no other runtime answers.
    pub async fn start(backend: B) -> Result<Self, Error> {
        // Checked before anything starts: two runtimes under one name would race in the CLI.
        let names = EnumMap::<S, String>::from_fn(|id| format!("{id:?}"));
        let distinct_names: BTreeSet<&str> = names.values().map(String::as_str).collect();
        assert_eq!(
            distinct_names.len(),
            names.len(),
            "two runtimes print the same name: {names:?}"
        );

        let started =
            join_enum_map(|id: S| start_runtime(backend.shim(id), names[id].clone(), id)).await;
        let runtimes = started.try_map(|_, runtime| {
            let runtime = runtime?;
            Ok::<_, Error>(Runtime {
                name: runtime.name().to_owned(),
                zid: runtime.zid(),
                running: Some(runtime),
            })
        })?;

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
        &self.runtimes[id]
    }

    /// every runtime with its id, in no particular order
    pub fn runtimes(&self) -> impl Iterator<Item = (S, &Runtime<B::Cli>)> {
        self.runtimes.iter()
    }

    /// the runtime `id`, to kill, restart or delete it
    pub fn runtime_mut(&mut self, id: S) -> RuntimeMut<'_, B, S> {
        let others = self
            .runtimes
            .iter()
            .filter(|(other, runtime)| *other != id && runtime.is_running())
            .map(|(_, runtime)| runtime.zid)
            .collect();
        RuntimeMut {
            id,
            runtime: &mut self.runtimes[id],
            backend: &self.backend,
            introspection: &self.introspection,
            others,
        }
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
                .runtimes
                .iter()
                .find(|(_, runtime)| RuntimeId::from(runtime.zid) == host)
                .unwrap_or_else(|| panic!("{sri} is placed on {host}, which is not in the swarm"));
            id
        }))
    }

    /// The runtime holding the db scope `scope`; for a cell's data (`scope_of_cell`) it need not
    /// be the cell's host. Call it only while no replication entry covers the scope: until then
    /// it has exactly one holder.
    pub async fn scope_holder(&self, scope: &Scope) -> S {
        let holders =
            db_client::replica_v1::Client::new(&self.session, Subject::Scope(scope.clone()))
                .expect("failed to open a db replica client")
                .locate(scope, None)
                .await
                .expect("failed to locate the scope");
        let [holder] = holders.as_slice() else {
            panic!("scope {scope:?} has {} holders, not one", holders.len());
        };
        let (id, _) = self
            .runtimes
            .iter()
            .find(|(_, runtime)| runtime.zid.to_le_bytes() == holder.id)
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
            .introspection
            .swarm_status()
            .await
            .expect("failed to query the swarm's status");
        check::mismatch(&statuses, &self.running_zids())
    }

    /// the zids of the runtimes that are running
    fn running_zids(&self) -> BTreeSet<ZenohId> {
        self.runtimes
            .values()
            .filter(|runtime| runtime.is_running())
            .map(Runtime::zid)
            .collect()
    }

    /// a zenoh session that is part of the swarm, e.g. for [`crate::clients::db::DbHandle`]
    pub fn session(&self) -> &zenoh::Session {
        &self.session
    }

    /// The CLI for the commands that act on the whole swarm, e.g. [`myrmic::Myrmic::cell`] to
    /// deploy a cell (see [`SwarmBackend::client`]).
    pub fn client(&self) -> myrmic::Myrmic<B::Client> {
        self.backend.client()
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
        self.backend.block(a, b).await;
    }

    /// Cut the link between the runtime `runtime` and the test's router (see
    /// [`Block::block_router`]).
    pub async fn block_router(&self, runtime: S) {
        self.backend.block_router(runtime).await;
    }
}

impl<B, S> Swarm<B, S>
where
    B: Isolate<S>,
    S: SwarmSpec,
{
    /// Cut every running runtime off the network at once (see [`RuntimeMut::isolate`]): each
    /// keeps running, alone.
    pub async fn isolate_all(&mut self) {
        for (id, runtime) in self.runtimes.iter() {
            if runtime.is_running() {
                self.backend.isolate(id).await;
            }
        }
    }

    /// Connect every running runtime again, each of which must be isolated (see
    /// [`Self::isolate_all`]). Returns once the swarm is connected again.
    ///
    /// For runtimes isolated together: [`RuntimeMut::heal`] of one of them waits for it to reach
    /// the others, which can't happen while they are still isolated.
    pub async fn heal_all(&mut self) -> Result<(), Error> {
        for (id, runtime) in self.runtimes.iter() {
            if runtime.is_running() {
                self.backend.heal(id).await;
            }
        }
        wait_until_connected(&self.introspection, &self.running_zids()).await
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

/// `myrmic runtimes start` of the runtime `id`, named `name`.
async fn start_runtime<B, S>(
    myrmic: myrmic::Myrmic<B>,
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
    B: SwarmBackend<S>,
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
    B: SwarmBackend<S>,
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
        let shim = self.backend.shim(self.id);
        self.start_on(shim).await?;
        let mut expected = self.others.clone();
        expected.insert(self.runtime.zid);
        wait_until_connected(self.introspection, &expected).await
    }

    /// Start the killed or deleted runtime under its name on `shim`.
    async fn start_on(&mut self, shim: myrmic::Myrmic<B::Cli>) -> Result<(), Error> {
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

    /// Freeze the running runtime (see [`myrmic::Runtime::pause`]). It keeps running, but the
    /// rest of the swarm hears nothing from it until [`Self::resume`].
    pub async fn pause(&mut self) -> Result<(), Error> {
        self.running().pause().await
    }

    /// Let the paused runtime run again. Returns once the swarm, with this runtime, is connected
    /// again.
    pub async fn resume(&mut self) -> Result<(), Error> {
        self.running().resume().await?;
        let mut expected = self.others.clone();
        expected.insert(self.runtime.zid);
        wait_until_connected(self.introspection, &expected).await
    }

    /// the running runtime
    fn running(&self) -> &myrmic::Runtime<B::Cli> {
        let id = self.id;
        self.runtime
            .running
            .as_ref()
            .unwrap_or_else(|| panic!("{id:?} is not running"))
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
    B: Isolate<S>,
    S: SwarmSpec,
{
    /// Cut the running runtime off the network (see [`Isolate::isolate`]). It keeps running, and
    /// the rest of the swarm only notices its silence.
    pub async fn isolate(&mut self) {
        let id = self.id;
        assert!(self.runtime.is_running(), "{id:?} is not running");
        self.backend.isolate(id).await;
    }

    /// Connect the isolated runtime again. Returns once the swarm, with this runtime, is
    /// connected again.
    pub async fn heal(&mut self) -> Result<(), Error> {
        self.backend.heal(self.id).await;
        let mut expected = self.others.clone();
        expected.insert(self.runtime.zid);
        wait_until_connected(self.introspection, &expected).await
    }
}

impl<B, S> RuntimeMut<'_, B, S>
where
    B: Readdress<S>,
    S: SwarmSpec,
{
    /// Move the running runtime to another address (see [`Readdress::readdress`]). Returns at
    /// once: finding back into the swarm is the runtime's own business.
    pub async fn readdress(&mut self) {
        let id = self.id;
        assert!(self.runtime.is_running(), "{id:?} is not running");
        self.backend.readdress(id).await;
    }
}

impl<B, S> RuntimeMut<'_, B, S>
where
    B: FillDisk<S>,
    S: SwarmSpec,
{
    /// Fill the disk the running runtime keeps its data on up to `room` free bytes (see
    /// [`FillDisk::fill_disk`]). It keeps running, and its writes start failing once the room is
    /// used up.
    pub async fn fill_disk(&mut self, room: u64) {
        let id = self.id;
        assert!(self.runtime.is_running(), "{id:?} is not running");
        self.backend.fill_disk(id, room).await;
    }

    /// Free the disk filled by [`Self::fill_disk`] again.
    pub async fn free_disk(&mut self) {
        self.backend.free_disk(self.id).await;
    }
}

impl<B, S> RuntimeMut<'_, B, S>
where
    B: SkewClock<S>,
    S: SwarmSpec,
{
    /// Set the running runtime's wall clock to `ahead_secs` seconds ahead of the others' (behind
    /// them if negative; 0 puts it back in step), as a clock step on its host would (see
    /// [`SkewClock::skew_clock`]). It keeps running.
    pub async fn skew_clock(&mut self, ahead_secs: i64) {
        let id = self.id;
        assert!(self.runtime.is_running(), "{id:?} is not running");
        self.backend.skew_clock(id, ahead_secs).await;
    }
}

impl<B, S> RuntimeMut<'_, B, S>
where
    B: CutOff<S>,
    S: SwarmSpec,
{
    /// Start the killed or deleted runtime again under its name, so with its zid and database,
    /// but cut off from the rest of the swarm (see [`CutOff`]). Returns once it is ready; no
    /// connectivity check, it reaches no other runtime. [`Self::delete`] or [`Self::kill`] it
    /// before a [`Self::restart`] brings it back into the swarm.
    pub async fn start_cut_off(&mut self) -> Result<(), Error> {
        let shim = self.backend.cut_off_shim(self.id);
        self.start_on(shim).await
    }
}
