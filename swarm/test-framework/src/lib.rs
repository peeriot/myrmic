//! Tools for end-to-end tests of myrmic. A test starts real runtimes on real machines, operates
//! them through the `myrmic` CLI as a user would, and injects the faults a deployed swarm can
//! meet: crashes, freezes, network splits, full disks and clock steps.
//!
//! # A first test
//!
//! ```ignore
//! use test_framework::{World, overlay::Overlay, runtime::Stopped, start_runtimes, swarm::Swarm};
//!
//! #[tokio::test(flavor = "multi_thread")]
//! async fn a_killed_runtime_comes_back() -> anyhow::Result<()> {
//!     // only the test's machine, and a multicast group that no other test uses
//!     let group = Overlay::new(&World::local()).new_multicast_group();
//!     let swarm = Swarm::new(&group).await;
//!     // two runtimes on the test's machine, started with `myrmic runtimes start`
//!     let a = swarm.add(swarm.get_myrmic().runtime());
//!     let b = swarm.add(swarm.get_myrmic().runtime());
//!     start_runtimes!(swarm; a, b); // both run and are linked to each other
//!
//!     let cell = swarm.get_myrmic().cell("cells/counter").deploy().await?;
//!     cell.send("inc", None).await?;
//!
//!     let a = a.stop_with(|rt| rt.kill()).await?; // a crash: no goodbye, no cleanup
//!     let a = a.start().await?; // returns once the swarm is connected again
//!     Ok(())
//! }
//! ```
//!
//! Tests clean up after themselves, also when they fail: dropping the handle of a running runtime
//! deletes the runtime, and dropping a deployed cell deletes the cell.
//!
//! Tests can run in parallel without disturbing each other. Each test uses a multicast group of its
//! own, and each CLI on the test's machine keeps its state in directories of its own.
//!
//! The sections below explain the parts of this test, starting with the machines and ending with
//! the swarm. After them come the faults a test can inject, and a swarm in docker containers.
//!
//! # Machines: hosts and the world
//!
//! Every runtime runs on a host ([`host`]). A host is the test's own machine ([`host::Local`]), a
//! docker container ([`host::Docker`]), or a machine the test reaches over SSH ([`host::Ssh`]).
//!
//! Hosts belong to host sets ([`host_set`]). A host set is a group of hosts that can dial each
//! other directly, for example a docker network ([`host_set::Docker`], which also creates the
//! containers on it) or a network of SSH machines ([`host_set::Ip`]). All host sets of a test
//! together form its [`World`]. The world answers whether one host can dial another, and how. A
//! test whose runtimes all run on the test's machine uses [`World::local`]. [`world!`] builds a
//! world from several host sets.
//!
//! # Links: the overlay
//!
//! The [`overlay::Overlay`] decides which runtimes link to each other over zenoh. It sits on top of
//! the world: a link only exists if the world lets one host dial the other. Every runtime becomes
//! a member of an overlay when it is created, in one of two ways:
//!
//! - on a multicast group of the overlay: the runtime links to every other member of the group;
//! - by a connect to one other member ([`overlay::Overlay::connect_to`]): that member is the
//!   runtime's only link.
//!
//! The test's own zenoh session, its CLI and a swarm attach to the overlay in the same two ways
//! ([`overlay::Attachment`]).
//!
//! # Runtimes
//!
//! A [`myrmic::Myrmic`] wraps the `myrmic` CLI on one host; these docs call it the shim. A test
//! gets one from the host: [`host::Local::myrmic_isolated`], [`host::Docker::myrmic`] or
//! [`host::Ssh::myrmic_isolated`]. The CLI operates a swarm: it deploys cells and apps, sends
//! commands, and reads status and telemetry. It also starts runtimes: [`myrmic::Myrmic::runtime`]
//! returns a runtime that is not started yet, and starting it runs `myrmic runtimes start`.
//!
//! A shim's type says where its CLI runs, by its backend: [`myrmic::Local`], [`myrmic::Docker`]
//! or [`myrmic::Ssh`], one for each kind of host. The backend is part of the type of every runtime
//! the shim starts. A running CLI runtime in a container, as a runtime of a swarm, is a
//! `swarm::RunningRt<myrmic::RunningRt<myrmic::Docker>>`. So three types are named `Docker`: the
//! container ([`host::Docker`]), the network ([`host_set::Docker`]) and the backend
//! ([`myrmic::Docker`]), and likewise for `Local`. The module path tells them apart.
//!
//! A runtime has two states, stopped and running, and each state is a type of its own
//! ([`runtime`]). Starting a runtime consumes the stopped value and returns a running one; stopping
//! it does the opposite. A call in the wrong state therefore doesn't compile.
//!
//! The traits [`runtime::Stopped`] and [`runtime::Running`] hold the transitions that every kind
//! of runtime has. A test imports them to call those methods. What only some kinds of runtimes can
//! do is a method of that kind, e.g. [`myrmic::RunningRt::kill`] (see [Faults](#faults)).
//!
//! The other kind of runtime is a node that runs inside the test process ([`in_process`]). A test
//! uses one for what a CLI runtime can't do: run only some plugins, run under a zid the test
//! chooses, or send its log events to the test process.
//!
//! # The swarm
//!
//! A [`swarm::Swarm`] holds the runtimes a test added to it, which can be of different kinds, and
//! what the test needs to operate them:
//!
//! - a CLI on the test's machine ([`swarm::Swarm::get_myrmic`]);
//! - for what the CLI doesn't cover: the swarm's sorg client ([`swarm::Swarm::get_sorg`]; sorg is
//!   myrmic's self-organization layer, which deploys and places cells and delivers commands), its
//!   db ([`swarm::Swarm::get_db`]), and a zenoh session that is part of the swarm
//!   ([`swarm::Swarm::get_session`]).
//!
//! The swarm checks its connectivity. When a test starts a runtime through the swarm, the start
//! only returns once exactly the running runtimes answer, and each of them is linked to exactly
//! the runtimes the overlay links it to. So a test can rely on its setup being the one it
//! described.
//!
//! A running runtime of the swarm dereferences to the handle of its own kind, so a test calls that
//! kind's methods directly (`a.pause()`). To stop the runtime in a way only its kind supports, a
//! test uses [`swarm::RunningRt::stop_with`].
//!
//! A cell is addressed by its SRI (stable resource identifier), which is derived from the cell's
//! name, its SRN. A test usually deploys a cell through the CLI ([`myrmic::Myrmic::cell`]). In
//! three cases it registers a class and deploys from it instead ([`swarm::cell`]): for many cells
//! of one class, for several cells in one request, and for AOT compilations the test built itself.
//! [`swarm::Swarm::get_host_of`] tells which runtime a cell runs on. A cell's data is in the db,
//! where [`swarm::DbHandle`] reads it.
//!
//! # Faults
//!
//! - **Crash:** [`myrmic::RunningRt::kill`] ends a CLI runtime without a goodbye. Starting the
//!   stopped handle brings the same runtime back.
//! - **Freeze:** [`myrmic::RunningRt::pause`] stops a CLI runtime's process, and
//!   [`myrmic::RunningRt::resume`] lets it continue. Through a swarm's handle, `resume` doesn't
//!   wait for the swarm; [`swarm::Swarm::wait_until_connected`] does.
//! - **Power loss:** cutting the power needs hardware that switches it, which this crate doesn't
//!   control. Once a runtime's host lost its power, [`myrmic::RunningRt::mark_lost`] turns the
//!   handle into a stopped one.
//! - **Network split, right away:** [`host::Docker::isolate`] cuts a docker host off its network,
//!   and [`host::Docker::heal`] undoes that. [`host::Docker::block`] cuts two docker hosts apart,
//!   and [`host::Block::unblock`] undoes that. [`host::Docker::readdress`] gives a docker host a
//!   new address.
//! - **Network split, at the next start:** [`overlay::Overlay::set_group`] moves a runtime of any
//!   host to another group, e.g. one of its own, and takes effect when the runtime starts the next
//!   time. Moving it back the same way undoes it.
//! - **Bad link:** [`host::Docker::degrade`] delays, loses or slows down the traffic a docker
//!   host sends to another, and [`host::Degraded::restore`] undoes that.
//! - **Full disk:** [`host::Docker::fill_disk`] fills the disk of a docker host's runtimes, and
//!   [`host::Docker::free_disk`] frees it.
//! - **Little memory:** [`host::Docker::set_memory_limit`] limits a docker host's memory. A host
//!   whose runtimes hold more data than fits in it keeps that data on a volume
//!   ([`host_set::Docker::new_volume_host`]).
//! - **Clock step:** [`host::Docker::skew_clock`] steps a docker host's clock, and
//!   `skew_clock(0)` puts it back in step.
//! - **Held back replication:** a CLI runtime whose test hooks (see [below](#test-hooks)) set a
//!   [`replication_delay`](::swarm::TestHooks::replication_delay) lets its copies lag behind.
//!
//! # Measuring
//!
//! - [`host::Docker::count_traffic`] counts the bytes between two docker hosts.
//! - [`myrmic::RunningRt::get_peak_memory`] reads the most memory a CLI runtime has held.
//! - [`telemetry::Files`] reads the metrics and spans runtimes export to files, e.g. how many
//!   replication messages a runtime received for a scope.
//!
//! # A swarm in docker
//!
//! Faults that act on a host's network, disk or clock need the runtimes in docker containers. A
//! docker network ([`host_set::Docker`]) creates the containers. The swarm is best attached
//! through a router on a container of its own ([`host::Router`]), so that faults on the runtimes'
//! hosts never cut the test off from the swarm.
//!
//! ```ignore
//! use test_framework::{docker, host_set, overlay::Overlay, start_runtimes, swarm::Swarm, world};
//!
//! let network = host_set::Docker::new(&docker::Daemon::connect()).await;
//! let overlay = Overlay::new(&world![network]);
//! let group = overlay.new_multicast_group();
//! let router = network.new_host().await.router(&group).await;
//! let swarm = Swarm::new(overlay.connect_to(&router)).await;
//! let a = swarm.add(network.new_host().await.myrmic(&group).runtime());
//! let b = swarm.add(network.new_host().await.myrmic(&group).runtime());
//! start_runtimes!(swarm; a, b);
//!
//! a.get_host().isolate().await; // the host of `a`'s container
//! swarm.wait_until_connected().await?; // `b` without `a`
//! ```
//!
//! # Waiting
//!
//! Most of what a test checks in a swarm only becomes true some time after the action that causes
//! it. So a test waits for it instead of checking once:
//!
//! - [`wait_for`] waits for a state and returns a value from it;
//! - [`assert_eventually!`] waits for a condition to become true, and fails the test if it
//!   doesn't;
//! - [`wait_until`] waits for a condition and returns whether it became true;
//! - [`wait::check_throughout`] checks that a state holds for a whole period.
//!
//! # The binaries under test
//!
//! Runtimes run the `myrmic` binary built in this workspace, and routers ([`host::Router`]) run
//! its `zenoh-router`. [`resolve_binary!`] finds the most recent build, or the binary an
//! environment variable points to. A test copies binaries to an SSH machine with
//! [`host::Ssh::upload`].
//!
//! # Test hooks
//!
//! Some states a test needs can't be brought about from outside a runtime. A runtime's copy of a
//! scope that lags behind the others is one: the other runtimes send their replication messages to
//! all holders at once, so no fault on a host or a link holds back what one runtime receives,
//! while the rest of its traffic flows. For such states, the runtime itself has test hooks: code
//! that misbehaves on purpose when its config asks for it. They are the `test_hooks` section of a
//! runtime's config ([`::swarm::TestHooks`]), which a test gives a CLI runtime with
//! [`myrmic::StoppedRt::test_hooks`].
//!
//! The hooks are behind the `swarm` crate's `test-hooks` feature, one feature for all of them.
//! Nothing in the workspace switches it on, so neither a release nor a plain `cargo build`
//! contains them. A test that uses them needs a `myrmic` built with the feature:
//! `cargo build --bin myrmic --features swarm/test-hooks`, as the test pipelines build it.
//! Starting a runtime with test hooks on a `myrmic` without them panics with that command. A
//! runtime without test hooks runs on either build.
//!
//! # Smaller pieces
//!
//! - [`myrmic::Myrmic::debug_listener`] streams what `myrmic telemetry debug` shows: the commands
//!   and events in cells' mailboxes (the queues that hold them until the cell handles them), and
//!   the runtimes' log records.
//! - [`myrmic::mqtt`] runs an MQTT broker in the test process, for cells that talk MQTT through a
//!   bridge.
//! - [`docker`] holds the docker images, containers and networks behind [`host::Docker`] and
//!   [`host_set::Docker`].

pub mod binary;
pub mod docker;
pub mod host;
pub mod in_process;
pub mod myrmic;
pub mod overlay;
pub mod runtime;
pub mod swarm;
pub mod telemetry;
pub mod wait;
mod world;

pub use wait::{wait_for, wait_until};
pub use world::*;

/// Run the cleanup `command` of the drop guard `guard`. A failure is reported, not raised, because
/// a panic during unwinding aborts the process and hides the original panic.
fn run_cleanup(guard: &str, mut command: std::process::Command) {
    let invocation = format!("{command:?}");
    match command.output() {
        Ok(output) if output.status.success() => {}
        Ok(output) => eprintln!(
            "{guard} drop-guard: {invocation} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(err) => eprintln!("{guard} drop-guard: failed to run {invocation}: {err}"),
    }
}

/// for [`start_runtimes!`], which expands to `futures::join!` in the caller's crate
#[doc(hidden)]
pub use futures as __futures;

/// Resolve a binary by name, built for the host or for a target triple, in the target directory
/// of the workspace test-framework is built in (see [`binary::resolve`]).
///
/// Resolution order:
/// 1. the `{NAME_UPPER}_BINARY` env var (e.g. `MYRMIC_BINARY`, `ZENOH_ROUTER_BINARY`), or
///    `{NAME_UPPER}_{TRIPLE_UPPER}_BINARY` for a triple
/// 2. `<workspace>/target/<profile>/<name>` for a pinned profile, otherwise the most recently
///    built `<name>` of any profile (`<workspace>/target/<triple>/…` for a triple)
///
/// `triple` and `profile` are optional, in that order:
/// - `resolve_binary!("myrmic")`: the host's newest build
/// - `resolve_binary!("myrmic", profile = "release")`: the host's release build
/// - `resolve_binary!("myrmic", triple = "aarch64-unknown-linux-gnu", profile = "dist")`
#[macro_export]
macro_rules! resolve_binary {
    ($name:expr $(, triple = $triple:expr)? $(, profile = $profile:expr)?) => {
        $crate::binary::resolve(
            $name,
            None::<&str> $(.or(Some($triple)))?,
            None::<&str> $(.or(Some($profile)))?,
        )
    };
}

/// Captured output of a finished command (on the host, over SSH or inside a container).
#[derive(Debug, Clone)]
pub struct CommandOutput {
    /// whether the command exited with code 0
    pub success: bool,
    /// captured stdout, lossily decoded as UTF-8
    pub stdout: String,
    /// captured stderr, lossily decoded as UTF-8
    pub stderr: String,
}

impl From<std::process::Output> for CommandOutput {
    fn from(output: std::process::Output) -> Self {
        Self {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}
