use super::{CutOff, SwarmBackend, cli_runtime};
use crate::{
    myrmic::{self, Myrmic},
    swarm2::SwarmSpec,
};

/// [`SwarmBackend`] that runs every runtime on the host, through one
/// [`Myrmic::local_isolated`] shim: its runtimes find each other on the shim's multicast group,
/// and nothing outside the swarm sees them.
pub struct Local {
    myrmic: Myrmic<myrmic::Local>,
}

impl Local {
    /// a backend with its own isolated shim
    pub fn new() -> Self {
        Self {
            myrmic: Myrmic::local_isolated(),
        }
    }
}

impl Default for Local {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: SwarmSpec> SwarmBackend<S> for Local {
    type Runtime = myrmic::StoppedRt<myrmic::Local>;
    type Client = myrmic::Local;

    /// on the one shim, whichever runtime it is
    fn runtime(&self, id: S) -> myrmic::StoppedRt<myrmic::Local> {
        cli_runtime(&self.myrmic, id)
    }

    /// the one shim: it already runs next to the test
    fn client(&self) -> Myrmic<myrmic::Local> {
        self.myrmic.clone()
    }

    async fn connect_session(&self) -> zenoh::Session {
        zenoh::open(self.myrmic.backend().zenoh_config())
            .await
            .expect("failed to open zenoh session")
    }
}

impl<S: SwarmSpec> CutOff<S> for Local {
    /// on a fresh [`Myrmic::cut_off`] of the one shim, whichever runtime it is
    fn cut_off_runtime(&self, id: S) -> myrmic::StoppedRt<myrmic::Local> {
        cli_runtime(&self.myrmic.cut_off(), id)
    }
}
