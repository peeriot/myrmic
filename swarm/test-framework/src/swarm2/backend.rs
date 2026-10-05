//! Where a swarm's runtimes run.

use std::future::Future;

use crate::myrmic::{Myrmic, MyrmicBackend};

pub mod local;

/// Where a [`super::Swarm`]'s runtimes run and how a test joins them.
///
/// Everything else (starting the runtimes, the connectivity check, the handles) is the same on
/// every backend and lives in [`super::Swarm`].
pub trait SwarmBackend {
    /// where the CLI of [`Self::shim`] runs
    type Cli: MyrmicBackend;

    /// the shim the runtime named `runtime` runs on
    fn shim(&self, runtime: &str) -> &Myrmic<Self::Cli>;

    /// open a zenoh session that is part of the swarm
    fn connect_session(&self) -> impl Future<Output = zenoh::Session> + Send;
}
