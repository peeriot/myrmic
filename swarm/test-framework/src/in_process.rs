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
//! use test_framework::{
//!     in_process::StoppedRt,
//!     network,
//!     overlay::Overlay,
//!     runtime::{Running, Stopped},
//! };
//!
//! let group = Overlay::new(&network::Local).new_multicast_group();
//! let rt = StoppedRt::new(&group).orchestration().db().start().await?; // running
//! let session = zenoh::open(group.get_zenoh_config()).await.unwrap(); // a peer on its group
//! let rt = rt.delete().await?; // stopped: the node has left the network
//! let rt = rt.start().await?; // running again, under the same zid
//! ```
//!
//! - [`StoppedRt`] is a builder: it describes the whole node (plugins, zid, mode), and
//!   [`runtime::Stopped::start`] assembles the node's config from it, with the group its overlay
//!   gives it.
//! - A new [`StoppedRt`] runs no plugin; each plugin method adds one with its default config.
//! - Its zid is random but fixed at [`StoppedRt::new`], so starting it again brings back the same
//!   node. [`StoppedRt::zid`] sets a chosen one, for tests where the zid itself matters (an
//!   orchestrator's zid is its leadership priority).
//! - [`StoppedRt::new`] makes the node a member of an [`crate::overlay::Overlay`], attached on a
//!   group or by a connect, and it links to exactly what the overlay says. Several runtimes as a swarm,
//!   with the swarm's connectivity check and CLI, are [`crate::swarm3`]'s job.
//! - Its database is in memory: every start begins empty. A test that needs data to survive a
//!   restart wants a [`crate::myrmic`] runtime with `persistent`.
//! - It has neither `kill` nor `pause` (unlike [`crate::myrmic::RunningRt`]): closing its zenoh
//!   session always says goodbye to the other nodes, and there is no process of its own to freeze.
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

use zenoh::config::{WhatAmI, ZenohId};

use crate::{network, overlay, runtime};

/// An in-process node that is not running: never started yet, or deleted. A builder for the node;
/// see [the module docs](self).
pub struct StoppedRt {
    zid: ZenohId,
    /// the runtime as a member of its overlay, which tells it its links at start
    ovl_member: overlay::Member,
    orchestration: bool,
    db: bool,
    execution: bool,
    /// a peer, unless [`Self::router_mode`] made it a router
    mode: WhatAmI,
}

impl StoppedRt {
    /// A node without plugins, under a random zid it keeps across starts, as a new member of an
    /// overlay, attached by `attachment`, on the test's machine.
    pub fn new(attachment: impl Into<overlay::Attachment>) -> Self {
        let attachment = attachment.into();
        Self {
            // `ZenohId`'s default is a random id
            zid: ZenohId::default(),
            ovl_member: attachment.add_member(network::HostId::TEST_MACHINE),
            orchestration: false,
            db: false,
            execution: false,
            mode: WhatAmI::Peer,
        }
    }

    /// the zid the node runs under; defaults to a random one
    pub fn zid(mut self, zid: ZenohId) -> Self {
        self.zid = zid;
        self
    }

    /// Run in zenoh's router mode instead of as a peer. It listens where every node does (see
    /// [`crate::overlay`]), not on zenoh's fixed 7447 for routers, which parallel tests would share.
    pub fn router_mode(mut self) -> Self {
        self.mode = WhatAmI::Router;
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
        let settings = self.ovl_member.get_zenoh_settings();
        let mut config = swarm::SwarmConfig {
            zenoh: zenoh::Config::from_json5(&settings.to_string())
                .expect("the overlay's zenoh settings are a valid config"),
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
            .zenoh
            .set_mode(Some(self.mode))
            .expect("setting zenoh mode cannot fail");
        config
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
            _ovl_running: self.ovl_member.start_running(self.mode),
            stopped: self,
            node,
        })
    }

    fn get_overlay_member(&self) -> &overlay::Member {
        &self.ovl_member
    }
}

/// A running in-process node; see [the module docs](self). Dropping it takes the node off the
/// network.
pub struct RunningRt {
    /// what it was started from, handed back by [`runtime::Running::delete`]
    stopped: StoppedRt,
    node: swarm::spawn::Spawned,
    /// records the node as running in its overlay while it exists
    _ovl_running: overlay::RunningGuard,
}

impl RunningRt {
    /// The node's own zenoh session, for what only works on a node's session, e.g. an
    /// introspection client's `on_join`, which needs the node's introspection plugin.
    pub fn get_session(&self) -> &zenoh::Session {
        self.node.session()
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
        // The overlay guard stays in `self`. Both go when this returns: first `node`, a local,
        // whose drop stops the plugins, then `self`, the parameter, with the guard.
        let RunningRt {
            stopped, mut node, ..
        } = self;
        node.kill_async().await;
        // closed already: dropping the handle only has to stop the plugins
        node.kill_on_drop = false;
        Ok(stopped)
    }
}
