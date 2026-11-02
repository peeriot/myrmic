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

    /// where the CLI of [`Self::client`] runs
    type Client: MyrmicBackend;

    /// the shim the runtime named `runtime` runs on
    fn shim(&self, runtime: &str) -> &Myrmic<Self::Cli>;

    /// the CLI the test acts on the whole swarm through: it runs next to the test, joins the
    /// swarm like [`Self::connect_session`] and runs no runtime itself
    fn client(&self) -> &Myrmic<Self::Client>;

    /// open a zenoh session that is part of the swarm
    fn connect_session(&self) -> impl Future<Output = zenoh::Session> + Send;
}

/// A [`SwarmBackend`] that can start a runtime cut off from the rest of its swarm (see
/// [`super::RuntimeMut::start_cut_off`]).
pub trait CutOff: SwarmBackend {
    /// A shim on which the runtime named `runtime` starts with its identity and database, but
    /// reaches no other runtime of the swarm, cut off or not.
    fn cut_off_shim(&self, runtime: &str) -> Myrmic<Self::Cli>;
}
