//! A runtime whose node runs inside the test process: the node library (the `swarm` crate, which
//! every `myrmic` runtime runs in its daemon) started directly, as tasks on the test's tokio
//! runtime, with no CLI in between.
//!
//! It is for tests that need what a CLI runtime cannot give them: a node with only some plugins
//! (e.g. an orchestrator without execution), a zid the test chooses, or the node's log events in
//! the test process, where a tracing subscriber of the test can watch them. For everything else,
//! a [`crate::myrmic`] runtime is closer to what users run.
//!
//! # Using it
//!
//! ```ignore
//! use test_framework::in_process::StoppedRt;
//! use test_framework::runtime::{Running, Stopped};
//!
//! let rt = StoppedRt::new().orchestration().db().start().await?; // running
//! let session = zenoh::open(rt.get_zenoh_config()).await?; // a peer on the node's group
//! let rt = rt.delete().await?; // stopped: the node has left the network
//! let rt = rt.start().await?; // running again, under the same zid
//! ```
//!
//! - [`StoppedRt`] is a builder: it describes the whole node (plugins, zid, multicast group), and
//!   [`runtime::Stopped::start`] assembles the node's config from it.
//! - A new [`StoppedRt`] runs no plugin; each plugin method adds one with its default config.
//! - Its zid is random but fixed at [`StoppedRt::new`], so starting it again brings back the same
//!   node. [`StoppedRt::zid`] sets a chosen one, for tests where the zid itself matters (an
//!   orchestrator's zid is its leadership priority).
//! - Each [`StoppedRt::new`] scouts on a multicast group of its own, so a node started by hand
//!   sees nothing else and nothing sees it. Nodes that should see each other get one group
//!   through [`StoppedRt::multicast_group`]; [`RunningRt::get_zenoh_config`] gives a session on
//!   the node's group. Several nodes as a swarm, with the swarm's connectivity check and CLI,
//!   are [`crate::swarm2::InProcess`]'s job.
//! - Its database is in memory: every start begins empty. A test that needs data to survive a
//!   restart wants a [`crate::myrmic`] runtime with `persistent`.
//! - It implements neither [`runtime::Kill`] nor [`runtime::Pause`]: closing its zenoh session
//!   always says goodbye to the other nodes, and there is no process of its own to freeze.
//! - Its log events go to the test process's tracing subscriber. A test that watches them (e.g.
//!   with sorg-tests' log tracker) installs its subscriber before the first start; otherwise the
//!   first start installs a plain one that writes to the test's output.
//!
//! # Design notes
//!
//! Deleting a node takes two steps, and the order matters. Dropping the node library's handle
//! ([`swarm::spawn::Spawned`]) stops the plugins, but two tasks the node library starts next to
//! them keep holding the zenoh session, and with it the node's place on the network. Only closing
//! the session and the zenoh runtime explicitly takes the node off the network, so
//! [`runtime::Running::delete`] does that first and then drops the handle. Dropping a
//! [`RunningRt`] does the same, synchronously.
//!
//! The node library installs a node's telemetry as the process's global tracing subscriber unless
//! one exists, bound to that node's session. With several nodes in one process, the first would
//! take the slot and keep it after it was deleted. Installing a subscriber before the first start
//! is the node library's intended use, and keeps every node out of that slot.
//!
//! [`runtime::Running::delete`] returns once the node has left the network, not once all its
//! plugin tasks have ended: their handles are private to the node library. With an in-memory
//! database nothing depends on them having ended.

use zenoh::config::ZenohId;

use crate::{myrmic::backend::isolated_multicast_group, runtime};

/// An in-process node that is not running: never started yet, or deleted. A builder for the node;
/// see [the module docs](self). A clone describes the same node, under the same zid.
#[derive(Clone)]
pub struct StoppedRt {
    zid: ZenohId,
    multicast_group: String,
    orchestration: bool,
    db: bool,
    execution: bool,
}

impl StoppedRt {
    /// A node without plugins, under a random zid it keeps across starts, on a multicast group of
    /// its own.
    pub fn new() -> Self {
        Self {
            // `ZenohId`'s default is a random id
            zid: ZenohId::default(),
            multicast_group: isolated_multicast_group(),
            orchestration: false,
            db: false,
            execution: false,
        }
    }

    /// the zid the node runs under; defaults to a random one
    pub fn zid(mut self, zid: ZenohId) -> Self {
        self.zid = zid;
        self
    }

    /// The multicast group (`<address>:<port>`) the node scouts on; defaults to one of its own.
    /// Nodes that should see each other get the same group.
    pub fn multicast_group(mut self, group: &str) -> Self {
        group.clone_into(&mut self.multicast_group);
        self
    }

    /// run the orchestration plugin
    pub fn orchestration(mut self) -> Self {
        self.orchestration = true;
        self
    }

    /// run the db plugin, with its database in memory
    pub fn db(mut self) -> Self {
        self.db = true;
        self
    }

    /// run the execution plugin
    pub fn execution(mut self) -> Self {
        self.execution = true;
        self
    }

    /// the node library's config for this node
    fn swarm_config(&self) -> swarm::SwarmConfig {
        let mut config = swarm::SwarmConfig {
            zenoh: peer_config(&self.multicast_group),
            ..Default::default()
        };
        config
            .zenoh
            .set_id(Some(self.zid))
            .expect("setting a zenoh id cannot fail");
        config.plugins.orchestration = self.orchestration.then(Default::default);
        config.plugins.db = self.db.then(Default::default);
        config.plugins.execution = self.execution.then(Default::default);
        config
    }
}

impl Default for StoppedRt {
    fn default() -> Self {
        Self::new()
    }
}

impl runtime::Stopped for StoppedRt {
    type Error = anyhow::Error;

    type Running = RunningRt;

    /// Returns once every plugin of the node reported ready (the node library waits up to
    /// [`swarm::PLUGIN_STARTUP_TIMEOUT`]); fails if one fails to start.
    async fn start(self) -> Result<RunningRt, anyhow::Error> {
        if let Err(err) = tracing_subscriber::fmt().with_test_writer().try_init() {
            // the only reason a test should meet: a subscriber is installed already, the test's
            // own or an earlier start's, and it stays
            assert!(
                swarm_telemetry::TelemetryConfig::has_global(),
                "failed to install a tracing subscriber for in-process nodes: {err}"
            );
        }
        let node = swarm::Swarm::new(self.swarm_config())
            .wait_in_place()
            .await?
            .kill_on_drop();
        Ok(RunningRt {
            stopped: self,
            node,
        })
    }
}

/// A running in-process node; see [the module docs](self). Dropping it takes the node off the
/// network.
pub struct RunningRt {
    /// what it was started from, handed back by [`runtime::Running::delete`]
    stopped: StoppedRt,
    node: swarm::spawn::Spawned,
}

impl RunningRt {
    /// The node's own zenoh session, for what only works on a node's session, e.g. an
    /// introspection client's `on_join`, which needs the node's introspection plugin.
    pub fn get_session(&self) -> &zenoh::Session {
        self.node.session()
    }

    /// a zenoh config for a peer session on the node's multicast group, to talk to the node
    pub fn get_zenoh_config(&self) -> zenoh::Config {
        peer_config(&self.stopped.multicast_group)
    }
}

impl runtime::Running for RunningRt {
    type Error = anyhow::Error;

    type Stopped = StoppedRt;

    fn get_zid(&self) -> ZenohId {
        self.stopped.zid
    }

    /// Close the node's zenoh session and runtime, which takes it off the network, then stop its
    /// plugins. Returns once it has left the network (see the module's design notes).
    async fn delete(self) -> Result<StoppedRt, anyhow::Error> {
        let RunningRt { stopped, mut node } = self;
        node.kill_async().await;
        // closed already: dropping the handle only has to stop the plugins
        node.kill_on_drop = false;
        drop(node);
        Ok(stopped)
    }
}

/// a zenoh peer config that scouts on `multicast_group`
fn peer_config(multicast_group: &str) -> zenoh::Config {
    let mut config = zenoh::Config::default();
    config
        .insert_json5(
            "scouting/multicast/address",
            &serde_json::json!(multicast_group).to_string(),
        )
        .expect("a multicast group is a valid scouting address");
    config
}
