//! Where a swarm's runtimes run.

use std::{future::Future, marker::PhantomData};

use enum_map::EnumMap;

use super::SwarmSpec;
use crate::{
    myrmic::{self, Myrmic, MyrmicBackend},
    runtime,
};

pub mod docker;
pub mod in_process;
pub mod local;
pub mod ssh;

/// The port `zenoh-router` listens on: zenoh's default for routers.
const ROUTER_PORT: u16 = 7447;

/// What the test decides about each CLI runtime of a swarm: the tags it starts with, and whether
/// it keeps its database across restarts (no `--tmp`). Shared by the backends that start their
/// runtimes through the CLI.
///
/// Stored by the runtimes' [`enum_map::Enum::into_usize`] index rather than as `EnumMap`s: whether an
/// `EnumMap` is `Send + Sync` depends on `S::Array`, which the compiler cannot see through in
/// generic code, and [`SwarmBackend`] requires both.
struct CliOptions<S> {
    tags: Box<[&'static [&'static str]]>,
    persistent: Box<[bool]>,
    spec: PhantomData<S>,
}

impl<S: SwarmSpec> CliOptions<S> {
    fn new(tags: EnumMap<S, &'static [&'static str]>, persistent: EnumMap<S, bool>) -> Self {
        Self {
            tags: tags.into_values().collect(),
            persistent: persistent.into_values().collect(),
            spec: PhantomData,
        }
    }

    /// The CLI runtime `id`, started through `shim` (see [`SwarmBackend::runtime`]): named after
    /// the id's `Debug` output, with its tags and persistence.
    fn runtime<B: MyrmicBackend>(&self, shim: &Myrmic<B>, id: S) -> myrmic::StoppedRt<B> {
        shim.runtime(self.tags[id.into_usize()])
            .name(&format!("{id:?}"))
            .persistent(self.persistent[id.into_usize()])
    }
}

/// A client session on the router at `endpoint`. A client, unlike a peer, connects to nothing but
/// its endpoint, so the session reaches the swarm only through the router.
async fn router_session(endpoint: &str) -> zenoh::Session {
    let mut config = zenoh::Config::default();
    config
        .set_mode(Some(zenoh::config::WhatAmI::Client))
        .expect("setting zenoh mode cannot fail");
    config
        .insert_json5(
            "connect/endpoints",
            &serde_json::json!([endpoint]).to_string(),
        )
        .unwrap_or_else(|err| panic!("invalid endpoint `{endpoint}`: {err}"));
    config
        .insert_json5("scouting/multicast/enabled", "false")
        .expect("disabling multicast scouting cannot fail");
    zenoh::open(config)
        .await
        .unwrap_or_else(|err| panic!("failed to connect to the router at `{endpoint}`: {err}"))
}

/// Where the runtimes of a [`super::Swarm`] of `S` run and how a test joins them.
///
/// Everything else (the connectivity check, tracking which runtimes run) is the same on every
/// backend and lives in [`super::Swarm`].
///
/// It is `Send + Sync` because the swarm's runtime handles share it and hold it across awaits.
pub trait SwarmBackend<S: SwarmSpec>: Send + Sync {
    /// A runtime of this backend, not running (see [`crate::runtime`]). Its errors take the
    /// swarm's connectivity check timing out ([`myrmic::Error::Timeout`]), which starting a swarm
    /// runtime waits for.
    type Runtime: runtime::Stopped<Error: From<myrmic::Error>>;

    /// where the CLI of [`Self::client`] runs
    type Client: MyrmicBackend;

    /// The runtime `id`, not started. Every call describes the same runtime, so starting any of
    /// them brings back the same one.
    fn runtime(&self, id: S) -> Self::Runtime;

    /// the CLI the test acts on the whole swarm through: it joins the swarm like
    /// [`Self::connect_session`] and runs no runtime itself
    fn client(&self) -> Myrmic<Self::Client>;

    /// open a zenoh session that is part of the swarm
    fn connect_session(&self) -> impl Future<Output = zenoh::Session> + Send;
}

/// A [`SwarmBackend`] that can start a runtime cut off from the rest of its swarm (see
/// [`super::StoppedRt::start_cut_off`]).
pub trait CutOff<S: SwarmSpec>: SwarmBackend<S> {
    /// The runtime `id`, not started, which starts with its identity and database but
    /// reaches no other runtime of the swarm, cut off or not.
    fn cut_off_runtime(&self, id: S) -> Self::Runtime;
}

/// A [`SwarmBackend`] that can isolate a running runtime: cut it off the network and connect it
/// again, while it keeps running (see [`super::Swarm::isolate`]).
pub trait Isolate<S: SwarmSpec>: SwarmBackend<S> {
    /// Drop all traffic between the runtime `id` and everything else: the other runtimes
    /// and the test. Dropped, not rejected, so to the others it falls silent.
    fn isolate(&self, id: S) -> impl Future<Output = ()> + Send;

    /// lift [`Self::isolate`] of the runtime `id`
    fn heal(&self, id: S) -> impl Future<Output = ()> + Send;
}

/// A [`SwarmBackend`] that can move a running runtime to another address on its network, as when a
/// host's address changes, without telling the runtime or the swarm (see
/// [`super::Swarm::readdress`]).
pub trait Readdress<S: SwarmSpec>: SwarmBackend<S> {
    /// Move the runtime `id` to another address. It keeps running.
    fn readdress(&self, id: S) -> impl Future<Output = ()> + Send;
}

/// A [`SwarmBackend`] that can fill the disk a running runtime keeps its data on, as when a host's
/// disk runs full (see [`super::Swarm::fill_disk`]).
pub trait FillDisk<S: SwarmSpec>: SwarmBackend<S> {
    /// Fill the disk the runtime `id` keeps its data on (its database, its logs) up to
    /// `room` free bytes. It keeps running.
    fn fill_disk(&self, id: S, room: u64) -> impl Future<Output = ()> + Send;

    /// lift [`Self::fill_disk`] of the runtime `id`
    fn free_disk(&self, id: S) -> impl Future<Output = ()> + Send;
}

/// A [`SwarmBackend`] that can move a running runtime's wall clock away from the others', as when a
/// host's clock is stepped (see [`super::Swarm::skew_clock`]). Its monotonic clock stays as
/// it is, like with a real step.
pub trait SkewClock<S: SwarmSpec>: SwarmBackend<S> {
    /// Set the wall clock of the runtime `id` to `ahead_secs` seconds ahead of the others'
    /// (behind them if negative; 0 puts it back in step). Returns once the runtime reads it.
    fn skew_clock(&self, id: S, ahead_secs: i64) -> impl Future<Output = ()> + Send;
}

/// A [`SwarmBackend`] that can cut single links while everything else stays connected, e.g. to
/// leave two runtimes reaching each other only through a third (see [`super::Swarm::block`]).
pub trait Block<S: SwarmSpec>: SwarmBackend<S> {
    /// Drop all traffic between the runtimes `a` and `b`.
    fn block(&self, a: S, b: S) -> impl Future<Output = ()> + Send;

    /// Drop all traffic between the runtime `id` and the router the test reaches the swarm
    /// through, so the test reaches it only through other runtimes.
    fn block_router(&self, id: S) -> impl Future<Output = ()> + Send;
}
