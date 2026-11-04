use super::{CutOff, SwarmBackend};
use crate::myrmic::{self, Myrmic};
use crate::swarm2::SwarmSpec;

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
    type Cli = myrmic::Local;
    type Client = myrmic::Local;

    /// the one shim, whichever runtime it is
    fn shim(&self, _runtime: S) -> Myrmic<myrmic::Local> {
        self.myrmic.clone()
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
    /// a fresh [`Myrmic::cut_off`] of the one shim, whichever runtime it is
    fn cut_off_shim(&self, _runtime: S) -> Myrmic<myrmic::Local> {
        self.myrmic.cut_off()
    }
}
