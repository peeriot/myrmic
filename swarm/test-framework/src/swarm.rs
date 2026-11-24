//! A swarm of runtimes for tests: the runtimes a test added to it, which the swarm checks are
//! linked the way their overlay says, and the tools to operate them as a whole.
//!
//! # Using it
//!
//! ```ignore
//! use test_framework::{
//!     World, in_process,
//!     overlay::Overlay,
//!     runtime::{Running, Stopped},
//!     start_runtimes,
//!     swarm::Swarm,
//! };
//!
//! let group = Overlay::new(&World::local()).new_multicast_group();
//! let swarm = Swarm::new(&group).await;
//! let a = swarm.add(swarm.get_myrmic().runtime().tags(&["gpu"])); // a CLI runtime
//! let b = swarm.add(in_process::StoppedRt::new(&group).orchestration().db());
//! start_runtimes!(swarm; a, b); // both run, and the swarm is connected
//! let b = b.stop_with(|rt| rt.delete()).await?; // stopped
//! let b = b.start().await?; // running again; returns once the swarm is connected again
//! ```
//!
//! - A test creates the runtimes itself. They can be of any kind, but must be members of the same
//!   overlay. The test hands each one to [`Swarm::add`], which returns the swarm's handle for it
//!   ([`StoppedRt`]). The swarm knows exactly the runtimes added to it.
//! - [`Swarm::new`] takes the way the test attaches to the overlay
//!   ([`crate::overlay::Attachment`]): on a group, or by a connect, e.g. to a router. The test's
//!   zenoh session, its CLI ([`Swarm::get_myrmic`]) and its sorg client ([`Swarm::get_sorg`])
//!   all attach this way. The connectivity check sends its status query on that session too, so
//!   only the runtimes the session reaches can answer.
//! - Starting a runtime returns once the swarm is connected: exactly the running runtimes that the
//!   session reaches answer the status query, and each of them is linked to exactly the runtimes
//!   the overlay links it to. A running runtime that the session doesn't reach (e.g. one moved to
//!   a group of its own with [`crate::overlay::Overlay::set_group`]) is expected not to answer.
//!   [`crate::start_runtimes!`] starts several runtimes side by side and checks once.
//!   [`StoppedRt::start_without_check`] starts one without the check.
//! - A running handle dereferences to the handle of the runtime's own kind, for what only that
//!   kind can do (`a.pause()`). To stop a runtime in a way only its kind supports, a test uses
//!   [`RunningRt::stop_with`].
//! - A test usually deploys cells through the CLI ([`Swarm::get_myrmic`]). For many cells of one
//!   class, for several cells in one request, or for AOT compilations the test built itself, it
//!   registers a class and deploys from it instead ([`cell`], [`Swarm::register`]).
//! - The runtime a cell runs on is reported as a zid ([`Swarm::get_host_of`]). A test compares it
//!   with the zids of its handles ([`crate::runtime::Running::get_zid`]).
//!
//! # Design notes
//!
//! The test owns the runtimes, not the swarm. That way a runtime's state is the type of its
//! handle, and one swarm can hold runtimes of different kinds. The swarm keeps one entry per
//! runtime (`RuntimeEntry`). The entry holds what doesn't change with the state (overlay
//! membership, zid). Whether the test has the runtime running is the overlay's record of whether
//! its member is up: set by a start, cleared when the running handle goes away. It is the test's intent, and
//! the connectivity check compares it with the runtimes that actually answer. That is how it
//! catches a runtime that died without the test stopping it.
//!
//! The links the check expects come from the overlay, not from the swarm, because the overlay is
//! the only place that says which runtime links to which (see [`crate::overlay`]).

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, SystemTime},
};

use cell_protocol::{CellInstance, PlacementEntry, RuntimeId, Sri};
use db_client::v1::models::Subject;
use sorg_common::{
    CellConfig, CellDeployment, DeployRequest, DeploymentError, ExecRuntimeInfo, RequirementTags,
    exec_registry::list_registered_execs, node_lease, supervision::SupervisionTiming,
};
use zenoh::config::ZenohId;

/// the scope a [`DbHandle`] reads and writes in
pub use db_client::v1::models::Scope;
pub use db_handle::{DbError, DbHandle};
pub use rt::{RunningRt, StoppedRt};

use crate::{
    myrmic::{self, Myrmic, cell::DeployedCell},
    overlay::{self, Overlay},
    runtime::Stopped,
    wait::{Timeout, wait_for},
};

pub mod cell;
mod check;
mod db_handle;
mod rt;

/// A swarm: the runtimes a test added to it, and what the test operates them with. See
/// [the module docs](self).
pub struct Swarm {
    shared: Arc<Shared>,
    sorg: sorg_client::Client,
    db: DbHandle,
    session: zenoh::Session,
    client: Myrmic<myrmic::Local>,
}

/// Why [`Swarm::register`] failed.
#[derive(Debug, thiserror::Error)]
pub enum RegisterError {
    /// the class registry refused the class, or couldn't be reached
    #[error(transparent)]
    Sorg(#[from] sorg_client::Error),
    /// the registered class didn't become readable in time
    #[error(transparent)]
    Timeout(#[from] Timeout),
}

/// How long [`Swarm::register`] waits for its class to become readable: a class registered under
/// load on the rack takes seconds to reach every node.
const CLASS_VISIBLE_TIMEOUT: Duration = Duration::from_mins(1);

/// What a swarm shares with its runtimes' handles.
struct Shared {
    overlay: Overlay,
    /// how the swarm's session attaches to the overlay, which decides which runtimes answer it
    attachment: overlay::Attachment,
    /// one per runtime added to the swarm, in the order they were added
    entries: Mutex<Vec<Arc<RuntimeEntry>>>,
    introspection: introspection_client::v1::Client,
}

/// What a [`Swarm`] knows about one of its runtimes, whatever state it is in.
struct RuntimeEntry {
    member: overlay::Member,
    /// set at the runtime's first start, the same on every later one
    zid: OnceLock<ZenohId>,
}

impl Swarm {
    /// A swarm with no runtime yet, whose test session, CLI and clients attach to an overlay by
    /// `attachment`. The test's machine must be in the overlay's world: the swarm checks its
    /// runtimes against what the test's session reaches from there.
    pub async fn new(attachment: impl Into<overlay::Attachment>) -> Self {
        let attachment = attachment.into();
        assert!(
            attachment
                .get_overlay()
                .world_has_host(&crate::host::get_local_id()),
            "the test's machine is in none of the host sets of the overlay's world"
        );
        let session = zenoh::open(attachment.get_zenoh_config())
            .await
            .expect("failed to open the test's zenoh session");
        Self {
            client: crate::host::Local.myrmic_isolated(attachment.clone()),
            shared: Arc::new(Shared {
                overlay: attachment.get_overlay().clone(),
                attachment,
                entries: Mutex::new(vec![]),
                introspection: introspection_client::v1::Client::new(session.clone()).await,
            }),
            sorg: sorg_client::Client::new(session.clone()),
            db: DbHandle::new(&session),
            session,
        }
    }

    /// Add the stopped runtime `rt` to the swarm. `rt` must be a member of the swarm's overlay.
    /// From now on, the test starts and stops the runtime with the returned handle.
    pub fn add<S: Stopped>(&self, rt: S) -> StoppedRt<S> {
        let member = rt.get_overlay_member().clone();
        assert!(
            self.shared.overlay.has_member(&member),
            "the runtime is not a member of the swarm's overlay"
        );
        let mut entries = self.shared.entries.lock().expect("not poisoned");
        // one runtime, one entry: a second handle of the same runtime (e.g. a clone of a CLI
        // runtime's stopped handle) would get a second intent flag for it
        assert!(
            entries.iter().all(|entry| entry.member != member),
            "{member:?} was added twice"
        );
        let entry = Arc::new(RuntimeEntry {
            member,
            zid: OnceLock::new(),
        });
        entries.push(entry.clone());
        StoppedRt::new(rt, entry, self.shared.clone())
    }

    /// Wait until exactly the runtimes the test has running answer the status query, and each of
    /// them is linked to exactly the runtimes the overlay links it to. Starting a runtime already
    /// does this. A test calls it after something else disconnected and reconnected runtimes.
    pub async fn wait_until_connected(&self) -> Result<(), Timeout> {
        self.shared.wait_until_connected().await
    }

    /// Why the swarm isn't connected right now, or `None` if it is (see
    /// [`Self::wait_until_connected`]). It sends one query and doesn't wait, so a test can use it
    /// to check that the swarm stays connected.
    pub async fn get_connection_mismatch(&self) -> Option<String> {
        self.shared.get_mismatch().await
    }

    /// The zid of the runtime the cell `sri` is placed on, or `None` while the cell has no
    /// placement or its placement names no host yet. Fails like [`Self::get_placement`]. Panics
    /// if that runtime isn't one the test added to the swarm, e.g. an embedded runtime that joined
    /// by itself (see [`Self::wait_for_runtime`]).
    ///
    /// A killed runtime keeps its placements until the swarm has declared it dead. So during a
    /// failover, this still names the killed runtime for a while.
    pub async fn get_host_of(&self, sri: &str) -> Result<Option<ZenohId>, sorg_client::Error> {
        let host = self
            .get_placement(sri)
            .await?
            .and_then(|placement| placement.kind.host());
        Ok(host.map(|host| {
            self.shared
                .started_zids()
                .into_iter()
                .find(|zid| RuntimeId::from(*zid) == host)
                .unwrap_or_else(|| panic!("{sri} is placed on {host}, which is not in the swarm"))
        }))
    }

    /// The zid of the runtime that holds the db scope `scope`. For a cell's data (`scope_of_cell`),
    /// this need not be the cell's host. Only call it while no replication entry covers the scope:
    /// until then, the scope has exactly one holder.
    pub async fn get_scope_holder(&self, scope: &Scope) -> ZenohId {
        let holders =
            db_client::replica_v1::Client::new(&self.session, &Subject::Scope(scope.clone()))
                .expect("failed to open a db replica client")
                .locate(scope, None)
                .await
                .expect("failed to locate the scope");
        let [holder] = holders.as_slice() else {
            panic!("scope {scope:?} has {} holders, not one", holders.len());
        };
        self.shared
            .started_zids()
            .into_iter()
            .find(|zid| zid.to_le_bytes() == holder.id)
            .unwrap_or_else(|| panic!("scope {scope:?} is held by a runtime not in the swarm"))
    }

    /// The placement row of the cell `sri`, or `None` while it has none.
    ///
    /// Fails while the runtime that holds the swarm's placement table is unreachable, until the
    /// swarm has moved the table (up to a minute after a runtime is lost, see the operating guide).
    /// A test that reads it while runtimes are lost or cut off has to retry.
    pub async fn get_placement(
        &self,
        sri: &str,
    ) -> Result<Option<PlacementEntry>, sorg_client::Error> {
        let sri: Sri = sri.parse().expect("a deployed cell's SRI is valid");
        self.sorg.get_placement(&sri).await
    }

    /// The instance row of the cell `sri`, or `None` while it has none. Fails like
    /// [`Self::get_placement`], for the instance table.
    pub async fn get_instance(
        &self,
        sri: &str,
    ) -> Result<Option<CellInstance>, sorg_client::Error> {
        let sri: Sri = sri.parse().expect("a deployed cell's SRI is valid");
        Ok(sorg_common::instance_registry::get_instance(&self.session, &sri).await?)
    }

    /// a zenoh session that is part of the swarm, e.g. for a client of a swarm service that
    /// [`Self::get_db`] and [`Self::get_sorg`] don't cover
    pub fn get_session(&self) -> &zenoh::Session {
        &self.session
    }

    /// the swarm's db on [`Self::get_session`], e.g. to read or write a cell's data directly
    pub fn get_db(&self) -> &DbHandle {
        &self.db
    }

    /// a sorg client on [`Self::get_session`], e.g. to deploy several cells in one request
    pub fn get_sorg(&self) -> &sorg_client::Client {
        &self.sorg
    }

    /// The swarm with its sorg client ([`Self::get_sorg`]) waiting up to `timeout` for each answer;
    /// otherwise the sorg client's own default (15 s) applies.
    ///
    /// A deploy is answered once the runtime that runs the cell has confirmed it, and the swarm's
    /// orchestration waits for that up to its own timeout (see
    /// [`myrmic::StoppedRt::orchestration`]). A request that should see the orchestration give up
    /// has to wait at least that long.
    #[must_use]
    pub fn sorg_query_timeout(mut self, timeout: Duration) -> Self {
        let mut config = sorg_client::Config::default();
        config.set_query_timeout(timeout);
        self.sorg = sorg_client::Client::new_with_config(self.session.clone(), config);
        self
    }

    /// The CLI on the test's machine, for the commands that act on the whole swarm, e.g.
    /// [`Myrmic::cell`] to deploy a cell. It starts no runtime unless the test uses it to start
    /// one.
    pub fn get_myrmic(&self) -> &Myrmic<myrmic::Local> {
        &self.client
    }

    /// The swarm with its CLI ([`Self::get_myrmic`]) waiting up to `timeout` in each call (see
    /// [`Myrmic::timeout`]).
    #[must_use]
    pub fn cli_timeout(mut self, timeout: Duration) -> Self {
        self.client = self.client.timeout(timeout);
        self
    }

    /// Wait up to `timeout` until a runtime with all of `tags` can take cells: it is registered
    /// with the orchestration and its liveness lease is fresh. This is for a runtime the test
    /// didn't add to the swarm, e.g. an embedded runtime that joins on its own. The swarm's own
    /// runtimes can take cells as soon as their start has returned.
    pub async fn wait_for_runtime(&self, tags: &[&str], timeout: Duration) -> Result<(), Timeout> {
        wait_for_exec_runtime(&self.session, tags, timeout).await
    }

    /// Register `binaries` as one class in the swarm's class registry, for cells to be deployed
    /// from.
    ///
    /// Returns once the class is readable, so that a deploy right afterwards finds it. The
    /// registration and a deploy's read of the class are separate transactions, and without the
    /// wait the read could land on a node the class hasn't reached yet.
    pub async fn register(&self, binaries: &cell::Binaries) -> Result<cell::Class, RegisterError> {
        let wasm = read_binary(&binaries.wasm).await;
        let mut aots = vec![];
        for (platform, path) in &binaries.aots {
            aots.push((*platform, read_binary(path).await));
        }
        let hash = self.sorg.add_class(&binaries.name, wasm, aots).await?;
        // a failing read ends the wait with its error
        wait_for(
            CLASS_VISIBLE_TIMEOUT,
            &format!("class {} to be readable", binaries.name),
            || async {
                match self.sorg.get_class_info(hash).await {
                    Ok(info) => info.map(|_| Ok(())),
                    Err(err) => Some(Err(err)),
                }
            },
        )
        .await??;
        Ok(cell::Class { hash })
    }

    /// Deploy a cell of `class` under `sri` on a runtime that has `tags` (on any runtime if there
    /// are none). This is the standalone deploy, which places one cell at a time. `init` is passed
    /// to the cell's init function (`#[myrmic_sdk::init]`), e.g. to set its starting state; with
    /// `None`, the function gets nothing.
    ///
    /// Dropping the returned handle deletes the cell through the swarm's CLI
    /// ([`Self::get_myrmic`]).
    pub async fn deploy(
        &self,
        class: &cell::Class,
        sri: Sri,
        tags: &[&str],
        init: Option<&[u8]>,
    ) -> Result<DeployedCell<myrmic::Local>, DeploymentError> {
        self.sorg
            .deploy_wasm_cell_with_arguments(
                sri,
                class.hash,
                RequirementTags::new(tags.to_vec()),
                init.map(<[u8]>::to_vec),
                None,
            )
            .await?;
        Ok(DeployedCell::new(
            self.client.clone(),
            sri.to_string(),
            None,
        ))
    }

    /// Deploy the cells of `deployments` in one request: the planner places them together. Returns
    /// a handle for each cell, in the order of `deployments`; dropping one deletes its cell, as
    /// with [`Self::deploy`].
    pub async fn deploy_all(
        &self,
        deployments: &[cell::Deployment<'_>],
    ) -> Result<Vec<DeployedCell<myrmic::Local>>, DeploymentError> {
        let cells = deployments
            .iter()
            .map(|deployment| {
                CellDeployment::new(
                    deployment.sri,
                    CellConfig::Wasm {
                        class: deployment.class.hash,
                    },
                )
                .with_tags(RequirementTags::new(deployment.tags.to_vec()))
            })
            .collect();
        self.sorg.deploy_cells(DeployRequest::new(cells)).await?;
        Ok(deployments
            .iter()
            .map(|deployment| {
                DeployedCell::new(self.client.clone(), deployment.sri.to_string(), None)
            })
            .collect())
    }
}

impl Shared {
    /// the zids of the runtimes that were started at least once
    fn started_zids(&self) -> Vec<ZenohId> {
        self.entries
            .lock()
            .expect("not poisoned")
            .iter()
            .filter_map(|entry| entry.zid.get().copied())
            .collect()
    }

    /// The runtimes that the test has running and the swarm's session reaches, each with those of
    /// them that the overlay links it to. This is what the status of a connected swarm shows. A
    /// running runtime that the session can't reach (e.g. one moved to a group of its own) can't
    /// answer, so it isn't expected to.
    fn expected_links(&self) -> BTreeMap<ZenohId, BTreeSet<ZenohId>> {
        let entries = self.entries.lock().expect("not poisoned");
        let running: Vec<&RuntimeEntry> = entries
            .iter()
            .filter(|entry| {
                entry.member.is_up()
                    && self
                        .overlay
                        .is_reachable_by(&self.attachment, &entry.member)
            })
            .map(Arc::as_ref)
            .collect();
        running
            .iter()
            .map(|entry| {
                let linked = running
                    .iter()
                    .filter(|other| {
                        other.member != entry.member
                            && self.overlay.are_linked(&entry.member, &other.member)
                    })
                    .map(|other| other.get_zid())
                    .collect();
                (entry.get_zid(), linked)
            })
            .collect()
    }

    /// Why the swarm's status doesn't match [`Self::expected_links`] right now, or `None` if it
    /// does. A failed status query also counts as a mismatch, not as an error: while links settle
    /// after a fault (e.g. a cut link that zenoh hasn't dropped yet), a query can time out.
    async fn get_mismatch(&self) -> Option<String> {
        match self.introspection.swarm_status().await {
            Ok(statuses) => check::mismatch(&statuses, &self.expected_links()),
            Err(err) => Some(format!("the status query failed: {err}")),
        }
    }

    /// Wait until the swarm's status matches [`Self::expected_links`], then wait
    /// [`LEADER_SETTLE_TIME`]. Fails with the last mismatch after [`CONNECTED_TIMEOUT`].
    ///
    /// The expected links are read again on every poll, because another runtime of the swarm may
    /// start or stop during the wait (e.g. two handles started side by side). A snapshot taken
    /// before would count that runtime as foreign, or as missing.
    async fn wait_until_connected(&self) -> Result<(), Timeout> {
        let mut last_mismatch = String::new();
        let polled = tokio::time::timeout(CONNECTED_TIMEOUT, async {
            loop {
                match self.get_mismatch().await {
                    None => return,
                    Some(mismatch) => last_mismatch = mismatch,
                }
                tokio::time::sleep(crate::wait::DEFAULT_POLL_INTERVAL).await;
            }
        })
        .await;
        polled.map_err(|_| Timeout {
            what: format!("the swarm to be connected; last: {last_mismatch}"),
            timeout: CONNECTED_TIMEOUT,
        })?;
        tokio::time::sleep(LEADER_SETTLE_TIME).await;
        Ok(())
    }
}

impl RuntimeEntry {
    /// The zid of a runtime the test has running. Panics for one that was never started: only a
    /// started runtime has a zid.
    fn get_zid(&self) -> ZenohId {
        *self
            .zid
            .get()
            .unwrap_or_else(|| panic!("{:?} was never started, so it has no zid", self.member))
    }
}

/// How long the swarm may take to match its expected links: the default wait, plus zenoh's link
/// lease (10 s by default) for a link that a fault cut. A silent link only drops once its lease
/// runs out, so a runtime keeps listing a peer it was cut off from for that long.
const CONNECTED_TIMEOUT: Duration =
    Duration::from_secs(crate::wait::DEFAULT_TIMEOUT.as_secs() + 10);

/// How long the orchestrators get to agree on a leader once the swarm is connected. Each
/// orchestrator learns of the others from join events. Until it has, an orchestrator that knows of
/// no lower zid considers itself the leader, and two leaders both handle a deploy, so the second
/// one answers "already deployed". Nothing reports which orchestrator considers itself the leader,
/// so this is a fixed wait.
const LEADER_SETTLE_TIME: Duration = Duration::from_secs(1);

async fn wait_for_exec_runtime(
    session: &zenoh::Session,
    tags: &[&str],
    timeout: Duration,
) -> Result<(), Timeout> {
    wait_for(
        timeout,
        &format!("an exec runtime with tags {tags:?} to be placeable"),
        || async {
            // `list_registered_execs` reads the same DB-backed exec registry the orchestrator
            // itself reads before placing cells (see `sorg_common::exec_registry`). A runtime's
            // own `TOPIC_EXEC_RUNTIMES` queryable is not equivalent: that queryable starts
            // answering as soon as the exec runtime's queryables are registered, which happens
            // *before* it registers itself in the DB-backed registry (see
            // `sorg-execution/src/spawn.rs`) — racing ahead of that registration let a
            // subsequent `load_cell` reach the orchestrator before it could see the runtime,
            // failing with `NoRuntimesAvailable`.
            // A failed read finds nothing this round: the registry and the leases are written
            // while runtimes come up, so reads can miss them for a while.
            let execs = list_registered_execs(session).await.ok()?;
            let fresh = fresh_lease_ids(session).await.ok()?;
            execs
                .iter()
                .any(|info| runtime_has_tags(info, tags) && fresh.contains(&info.id()))
                .then_some(())
        },
    )
    .await
}

fn runtime_has_tags(info: &ExecRuntimeInfo, tags: &[&str]) -> bool {
    tags.iter()
        .all(|t| info.capabilities().tags().iter().any(|c| c.as_ref() == *t))
}

/// The runtimes whose liveness lease is fresh enough for placement to keep them. This applies the
/// same staleness rule as `drop_stale_execs` in the orchestrator: a node is fresh while the time
/// since its last renewal (`seq`, wall-clock milliseconds) stays within its declared `ttl_ms` plus
/// the cluster margin. A `seq` ahead of the reader's clock (clock skew) counts as fresh. A node
/// without a lease row is not in the set.
async fn fresh_lease_ids(
    session: &zenoh::Session,
) -> Result<HashSet<RuntimeId>, sorg_common::Error> {
    let leases = node_lease::list_leases(session).await?;
    let now_ms = u64::try_from(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("the clock is after the epoch")
            .as_millis(),
    )
    .expect("the time in milliseconds fits in u64");
    let margin_ms = u64::try_from(SupervisionTiming::default().margin.as_millis())
        .expect("the cluster margin in milliseconds fits in u64");

    let fresh = leases
        .into_iter()
        .filter(|(_, lease)| {
            now_ms.saturating_sub(lease.seq) <= lease.ttl_ms.saturating_add(margin_ms)
        })
        .map(|(id, _)| id)
        .collect();

    Ok(fresh)
}

/// The contents of the cell binary at `path`. Panics if it can't be read: the test built or named
/// it.
async fn read_binary(path: &std::path::Path) -> Vec<u8> {
    tokio::fs::read(path)
        .await
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use cell_protocol::{CapabilityTag, ExecutionCapabilities};

    use sorg_common::ExecRuntimeInfo;

    use super::runtime_has_tags;

    fn runtime_with_tags(tags: &[&str]) -> ExecRuntimeInfo {
        let tags = tags.iter().map(|t| CapabilityTag::new(*t)).collect();
        ExecRuntimeInfo::new(
            zenoh::config::ZenohId::default(),
            None,
            ExecutionCapabilities::new(tags),
        )
    }

    #[test]
    fn no_required_tags_matches_any_runtime() {
        assert!(runtime_has_tags(&runtime_with_tags(&[]), &[]));
        assert!(runtime_has_tags(&runtime_with_tags(&["linux"]), &[]));
    }

    #[test]
    fn matches_when_runtime_has_all_required_tags() {
        assert!(runtime_has_tags(
            &runtime_with_tags(&["linux", "gpu"]),
            &["linux"]
        ));
    }

    #[test]
    fn does_not_match_when_a_required_tag_is_missing() {
        assert!(!runtime_has_tags(&runtime_with_tags(&["linux"]), &["gpu"]));
    }
}
