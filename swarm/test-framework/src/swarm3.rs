//! A swarm of runtimes for tests: the runtimes a test added to it, checked to be linked as their
//! overlay says, and operated as a whole.
//!
//! # Using it
//!
//! ```ignore
//! use test_framework::{
//!     World, in_process, overlay::Overlay, runtime::Stopped, start_runtimes, swarm3::Swarm,
//! };
//!
//! let group = Overlay::new(&World::local()).new_multicast_group();
//! let swarm = Swarm::new(&group).await;
//! let a = swarm.add(swarm.get_myrmic().runtime().tags(&["gpu"]));  // a CLI runtime
//! let b = swarm.add(in_process::StoppedRt::new(&group).orchestration().db());
//! start_runtimes!(swarm; a, b);  // both running, the swarm connected
//! let b = b.stop_with(|rt| rt.delete()).await?;  // stopped
//! let b = b.start().await?;  // running again; returns once the swarm is connected again
//! ```
//!
//! - A test builds the runtimes itself, of any kind and attached to one overlay, and hands each
//!   to [`Swarm::add`], which returns the swarm's handle for it ([`StoppedRt`]). The swarm knows
//!   exactly the runtimes added to it.
//! - [`Swarm::new`] takes the way the test attaches to the overlay
//!   ([`crate::overlay::Attachment`]): the test's session, its CLI ([`Swarm::get_myrmic`]) and its
//!   clients ([`Swarm::get_sorg`]) attach that way, on a group or by a connect (e.g. to a router).
//!   The status query of the connectivity check goes out on that session too, so only the
//!   runtimes it reaches can answer.
//! - Starting a handle returns once the swarm is connected: exactly the running runtimes the
//!   session reaches answer the status query, each linked to exactly the runtimes the overlay
//!   links it to. A running runtime the session doesn't reach (e.g. one moved to a group of its
//!   own with [`crate::overlay::Overlay::set_group`]) is expected to stay silent.
//!   [`crate::start_runtimes!`] starts several side by side and checks once;
//!   [`StoppedRt::start_without_check`] starts one without the check.
//! - A running handle derefs to the runtime kind's own handle, for what only that kind can do
//!   (`a.pause()`); a kind's own way to stop goes through [`RunningRt::stop_with`].
//! - Which runtime a cell is on comes back as a zid ([`Swarm::get_host_of`]); a test compares it
//!   with its handles' [`crate::runtime::Running::get_zid`].
//!
//! # Design notes
//!
//! The swarm does not own its runtimes: the test does, so that a runtime's state is its handle's
//! type, and a swarm can hold runtimes of different kinds. The swarm keeps one entry per runtime
//! (`RuntimeEntry`) with what does not change with the state (overlay membership, zid) and
//! whether the test has the runtime running. That flag is the test's intent, set by a start and
//! cleared when the running handle goes away; the connectivity check compares it with the
//! runtimes that actually answer, which is what catches a runtime that died without the test
//! stopping it. How the handles keep that flag is in the handles' module (`rt`, re-exported
//! here).
//!
//! The links the check expects come from the overlay, not from the swarm: the overlay is the one
//! place that says which runtime links to which (see [`crate::overlay`]).
//!
//! The name is temporary: this replaces [`crate::swarm`] and takes over its name once that is
//! gone.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use cell_protocol::{CellInstance, PlacementEntry, RuntimeId, Sri};
use db_client::v1::models::{Scope, Subject};
use zenoh::config::ZenohId;

pub use rt::{RunningRt, StoppedRt};

use crate::{
    myrmic::{self, Error, Myrmic},
    overlay::{self, Overlay},
    runtime::Stopped,
    wait::Timeout,
};

mod check;
mod rt;

/// A swarm: the runtimes a test added to it, and what the test operates them with. See
/// [the module docs](self).
pub struct Swarm {
    shared: Arc<Shared>,
    sorg: sorg_client::Client,
    session: zenoh::Session,
    client: Myrmic<myrmic::Local>,
}

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
    /// whether the test has the runtime running: set by a start, cleared when the running handle
    /// goes away
    running: AtomicBool,
}

impl Swarm {
    /// A swarm with no runtime yet, whose test session, CLI and clients attach to an overlay by
    /// `attachment`.
    pub async fn new(attachment: impl Into<overlay::Attachment>) -> Self {
        let attachment = attachment.into();
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
            session,
        }
    }

    /// Make the stopped runtime `rt`, a member of the swarm's overlay, a runtime of the swarm; the
    /// returned handle is the one the test starts and stops it with from now on.
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
            running: AtomicBool::new(false),
        });
        entries.push(entry.clone());
        StoppedRt::new(rt, entry, self.shared.clone())
    }

    /// Wait until exactly the runtimes the test has running answer the status query, each linked
    /// to exactly the runtimes the overlay links it to. Starting a runtime already does this; a
    /// test calls it after something else that disconnects and reconnects runtimes.
    pub async fn wait_until_connected(&self) -> Result<(), Error> {
        self.shared.wait_until_connected().await
    }

    /// Why the swarm is not connected right now, or `None` if it is (see
    /// [`Self::wait_until_connected`]). One query, no waiting, so a test can check that the swarm
    /// stays connected.
    pub async fn get_connection_mismatch(&self) -> Option<String> {
        self.shared.get_mismatch().await
    }

    /// The zid of the runtime the cell `sri` is placed on, or `None` while it has no placement or
    /// its placement names no host yet. Fails like [`Self::get_placement`].
    ///
    /// A killed runtime keeps its placements until the swarm has declared it dead, so during a
    /// failover this still names the killed runtime for a while.
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

    /// The zid of the runtime holding the db scope `scope`; for a cell's data (`scope_of_cell`) it
    /// need not be the cell's host. Call it only while no replication entry covers the scope:
    /// until then it has exactly one holder.
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
    /// Fails while the runtime holding the swarm's placement table is unreachable, until the
    /// swarm has moved the table (up to a minute after a runtime is lost, see the operating
    /// guide). A test that reads it while runtimes are lost or cut off retries.
    pub async fn get_placement(
        &self,
        sri: &str,
    ) -> Result<Option<PlacementEntry>, sorg_client::Error> {
        let sri: Sri = sri.parse().expect("a deployed cell's SRI is valid");
        self.sorg.get_placement(&sri).await
    }

    /// Every placement row, one per placed cell. Fails like [`Self::get_placement`].
    pub async fn get_placements(&self) -> Result<Vec<PlacementEntry>, sorg_client::Error> {
        self.sorg.list_placements().await
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

    /// a zenoh session that is part of the swarm, e.g. for [`crate::clients::db::DbHandle`]
    pub fn get_session(&self) -> &zenoh::Session {
        &self.session
    }

    /// a sorg client on [`Self::get_session`], e.g. to deploy several cells in one request
    pub fn get_sorg(&self) -> &sorg_client::Client {
        &self.sorg
    }

    /// The CLI on the test's machine for the commands that act on the whole swarm, e.g.
    /// [`Myrmic::cell`] to deploy a cell. It starts no runtime unless the test has it start one.
    pub fn get_myrmic(&self) -> &Myrmic<myrmic::Local> {
        &self.client
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

    /// The runtimes the test has running and the swarm's session reaches, each with those of them
    /// the overlay links it to: what a connected swarm's status shows. A running runtime the
    /// session can't reach (e.g. one moved to a group of its own) can't answer, so it isn't
    /// expected to.
    fn expected_links(&self) -> BTreeMap<ZenohId, BTreeSet<ZenohId>> {
        let entries = self.entries.lock().expect("not poisoned");
        let running: Vec<&RuntimeEntry> = entries
            .iter()
            .filter(|entry| {
                entry.running.load(Ordering::SeqCst)
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
    /// does. A status query that fails is a mismatch too, not an error: while links settle after a
    /// fault (e.g. a cut link that zenoh hasn't dropped yet), a query can time out.
    async fn get_mismatch(&self) -> Option<String> {
        match self.introspection.swarm_status().await {
            Ok(statuses) => check::mismatch(&statuses, &self.expected_links()),
            Err(err) => Some(format!("the status query failed: {err}")),
        }
    }

    /// Wait until the swarm's status matches [`Self::expected_links`], then
    /// [`LEADER_SETTLE_TIME`]. Fails with the last mismatch after [`CONNECTED_TIMEOUT`].
    ///
    /// The expected links are read anew on every poll: another runtime of the swarm may start or
    /// stop while this one waits (e.g. two handles started side by side), and a snapshot taken
    /// before would count it as foreign, or as missing.
    async fn wait_until_connected(&self) -> Result<(), Error> {
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
/// lease (its default, 10 s) for a link a fault cut. A silent link only drops once its lease
/// runs out, so a runtime keeps listing a peer that a fault cut it from for that long.
const CONNECTED_TIMEOUT: Duration =
    Duration::from_secs(crate::wait::DEFAULT_TIMEOUT.as_secs() + 10);

/// How long the orchestrators get to agree on a leader once the swarm is connected. Each one
/// learns of the others from join events, and until it has, one that knows no lower zid takes
/// itself for the leader: two leaders both handle a deploy, and the second answers "already
/// deployed". Nothing reports who considers itself leader, so this is a fixed wait.
const LEADER_SETTLE_TIME: Duration = Duration::from_secs(1);
