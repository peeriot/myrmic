use std::future::Future;

use super::{CutOff, SwarmBackend};
use crate::myrmic::{LocalBinary, Myrmic};

/// [`SwarmBackend`] that runs every runtime on the host, through one
/// [`Myrmic::local_isolated`] shim: its runtimes find each other on the shim's multicast group,
/// and nothing outside the swarm sees them.
pub struct Local {
    myrmic: Myrmic<LocalBinary>,
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

impl SwarmBackend for Local {
    type Cli = LocalBinary;
    type Client = LocalBinary;

    /// the one shim, whichever runtime it is
    fn shim(&self, _runtime: &str) -> &Myrmic<LocalBinary> {
        &self.myrmic
    }

    /// the one shim: it already runs next to the test
    fn client(&self) -> &Myrmic<LocalBinary> {
        &self.myrmic
    }

    fn connect_session(&self) -> impl Future<Output = zenoh::Session> + Send {
        self.myrmic.connect_session()
    }
}

impl CutOff for Local {
    /// a fresh [`Myrmic::cut_off`] of the one shim, whichever runtime it is
    fn cut_off_shim(&self, _runtime: &str) -> Myrmic<LocalBinary> {
        self.myrmic.cut_off()
    }
}
