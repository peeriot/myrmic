//! Where a swarm's runtimes run.

use std::future::Future;

use super::SwarmSpec;
use crate::myrmic::{Myrmic, MyrmicBackend};

pub mod docker;
pub mod local;
pub mod ssh;

/// The port `zenoh-router` listens on: zenoh's default for routers.
const ROUTER_PORT: u16 = 7447;

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
/// Everything else (starting the runtimes, the connectivity check, the handles) is the same on
/// every backend and lives in [`super::Swarm`].
pub trait SwarmBackend<S: SwarmSpec> {
    /// where the CLI of [`Self::shim`] runs
    type Cli: MyrmicBackend;

    /// where the CLI of [`Self::client`] runs
    type Client: MyrmicBackend;

    /// the shim the runtime `runtime` runs on
    fn shim(&self, runtime: S) -> Myrmic<Self::Cli>;

    /// the CLI the test acts on the whole swarm through: it joins the swarm like
    /// [`Self::connect_session`] and runs no runtime itself
    fn client(&self) -> Myrmic<Self::Client>;

    /// open a zenoh session that is part of the swarm
    fn connect_session(&self) -> impl Future<Output = zenoh::Session> + Send;
}

/// A [`SwarmBackend`] that can start a runtime cut off from the rest of its swarm (see
/// [`super::RuntimeMut::start_cut_off`]).
pub trait CutOff<S: SwarmSpec>: SwarmBackend<S> {
    /// A shim on which the runtime `runtime` starts with its identity and database, but reaches
    /// no other runtime of the swarm, cut off or not.
    fn cut_off_shim(&self, runtime: S) -> Myrmic<Self::Cli>;
}

/// A [`SwarmBackend`] that can isolate a running runtime: cut it off the network and connect it
/// again, while it keeps running (see [`super::RuntimeMut::isolate`]).
pub trait Isolate<S: SwarmSpec>: SwarmBackend<S> {
    /// Drop all traffic between the runtime `runtime` and everything else: the other runtimes
    /// and the test. Dropped, not rejected, so to the others it falls silent.
    fn isolate(&self, runtime: S) -> impl Future<Output = ()> + Send;

    /// lift [`Self::isolate`] of the runtime `runtime`
    fn heal(&self, runtime: S) -> impl Future<Output = ()> + Send;
}

/// A [`SwarmBackend`] that can move a running runtime to another address on its network, as when a
/// host's address changes, without telling the runtime or the swarm (see
/// [`super::RuntimeMut::readdress`]).
pub trait Readdress<S: SwarmSpec>: SwarmBackend<S> {
    /// Move the runtime `runtime` to another address. It keeps running.
    fn readdress(&self, runtime: S) -> impl Future<Output = ()> + Send;
}

/// A [`SwarmBackend`] that can fill the disk a running runtime keeps its data on, as when a host's
/// disk runs full (see [`super::RuntimeMut::fill_disk`]).
pub trait FillDisk<S: SwarmSpec>: SwarmBackend<S> {
    /// Fill the disk the runtime `runtime` keeps its data on (its database, its logs) up to
    /// `room` free bytes. It keeps running.
    fn fill_disk(&self, runtime: S, room: u64) -> impl Future<Output = ()> + Send;

    /// lift [`Self::fill_disk`] of the runtime `runtime`
    fn free_disk(&self, runtime: S) -> impl Future<Output = ()> + Send;
}

/// A [`SwarmBackend`] that can cut single links while everything else stays connected, e.g. to
/// leave two runtimes reaching each other only through a third (see [`super::Swarm::block`]).
pub trait Block<S: SwarmSpec>: SwarmBackend<S> {
    /// Drop all traffic between the runtimes `a` and `b`.
    fn block(&self, a: S, b: S) -> impl Future<Output = ()> + Send;

    /// Drop all traffic between the runtime `runtime` and the router the test reaches the swarm
    /// through, so the test reaches it only through other runtimes.
    fn block_router(&self, runtime: S) -> impl Future<Output = ()> + Send;
}
