//! A runtime whose node runs inside the test process. The test starts the node library (the
//! `swarm` crate, which the daemon of every `myrmic` runtime runs) directly, as tasks on the
//! test's tokio runtime, with no CLI in between.
//!
//! A test uses it for what a CLI runtime can't give: a node with only some plugins (e.g. an
//! orchestrator without execution), a zid the test chooses, or the node's log events in the test
//! process, where a tracing subscriber of the test can watch them. For everything else, a
//! [`crate::myrmic`] runtime is closer to what users run.
//!
//! # Using it
//!
//! ```ignore
//! use test_framework::{
//!     World, in_process,
//!     overlay::Overlay,
//!     runtime::{Running, Stopped},
//! };
//!
//! let group = Overlay::new(&World::local()).new_multicast_group();
//! let rt = in_process::StoppedRt::new(&group).orchestration().db().start().await?; // running
//! let session = zenoh::open(group.get_zenoh_config()).await.unwrap(); // a peer on its group
//! let rt = rt.delete().await?; // stopped: the node has left the network
//! let rt = rt.start().await?; // running again, under the same zid
//! ```
//!
//! - [`StoppedRt`] is a builder that describes the whole node: its plugins, its zid and its mode.
//!   [`runtime::Stopped::start`] builds the node's config from it, with the zenoh settings the
//!   overlay gives the node.
//! - A new [`StoppedRt`] runs no plugin. Each plugin method adds one plugin with its default
//!   config.
//! - [`StoppedRt::new`] picks a random zid, and the node keeps it, so starting it again brings
//!   back the same node. [`StoppedRt::zid`] sets a zid of the test's choice, for tests where the
//!   zid itself matters (an orchestrator's zid is its priority for leadership).
//! - [`StoppedRt::new`] makes the node a member of an [`crate::overlay::Overlay`], on a group or
//!   by a connect, and the node links to exactly what the overlay says. To run several runtimes as
//!   a swarm, with the connectivity check and a CLI, a test uses [`crate::swarm`].
//! - Its database is in memory, so every start begins empty. A test that needs data to survive a
//!   restart uses a [`crate::myrmic`] runtime with `persistent`.
//! - Unlike [`crate::myrmic::RunningRt`], it has neither `kill` nor `pause`: closing its zenoh
//!   session always says goodbye to the other nodes, and there is no process of its own to freeze.
//! - Its log events go to the test process's tracing subscriber. A test that watches them (e.g.
//!   with sorg-tests' log tracker) installs its subscriber before the first start. Otherwise the
//!   first start installs a plain subscriber that writes to the test's output.
//!
//! # Design notes
//!
//! Deleting a node takes two steps, and their order matters. Dropping the node library's handle
//! ([`swarm::spawn::Spawned`]) stops the plugins. But two other tasks that the node library
//! starts keep holding the zenoh session, and so keep the node on the network. Only closing the
//! session and the zenoh runtime explicitly takes the node off the network. So
//! [`runtime::Running::delete`] closes them first and then drops the handle. Dropping a
//! [`RunningRt`] does the same, synchronously.
//!
//! Unless the process already has a global tracing subscriber, the node library installs the
//! node's telemetry as that subscriber, bound to the node's session. With several nodes in one
//! process, the first node would take that place and keep it after it was deleted. Installing a
//! subscriber before the first start is how the node library is meant to be used, and it keeps
//! every node out of that place.
//!
//! [`runtime::Running::delete`] returns once the node has left the network, not once all its
//! plugin tasks have ended, because their handles are private to the node library. With an
//! in-memory database, nothing depends on them having ended.

use zenoh::config::{WhatAmI, ZenohId};

use crate::{host, overlay, runtime};

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
    /// A node without plugins on the test's machine, as a new member of an overlay, attached by
    /// `attachment`. It gets a random zid, which it keeps across starts.
    pub fn new(attachment: impl Into<overlay::Attachment>) -> Self {
        let attachment = attachment.into();
        Self {
            // `ZenohId`'s default is a random id
            zid: ZenohId::default(),
            ovl_member: attachment.add_member(host::get_local_id()),
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

    /// Run in zenoh's router mode instead of as a peer. The node listens where every node does (see
    /// [`crate::overlay`]), not on zenoh's fixed router port 7447, which tests running in parallel
    /// would share.
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

    /// Returns once every plugin of the node has reported ready (the node library waits up to
    /// [`swarm::PLUGIN_STARTUP_TIMEOUT`]); fails if a plugin fails to start.
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
            _ovl_up: self.ovl_member.come_up(self.mode),
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
    _ovl_up: overlay::UpGuard,
}

impl RunningRt {
    /// The node's own zenoh session, for what only works on a node's session, e.g. an introspection
    /// client's `on_join`, which needs the node's introspection plugin.
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

    /// Close the node's zenoh session and runtime, which takes the node off the network, then stop
    /// its plugins. Returns once the node has left the network (see the module's design notes).
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
