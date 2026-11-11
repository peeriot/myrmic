use enum_map::EnumMap;

use super::{CliOptions, CutOff, SwarmBackend};
use crate::{
    myrmic::{self, Myrmic},
    swarm2::SwarmSpec,
};

/// [`SwarmBackend`] that runs every runtime on the host, through one
/// [`Myrmic::local_isolated`] shim: its runtimes find each other on the shim's multicast group,
/// and nothing outside the swarm sees them.
pub struct Local<S> {
    myrmic: Myrmic<myrmic::Local>,
    options: CliOptions<S>,
}

impl<S: SwarmSpec> Local<S> {
    /// A backend with its own isolated shim, whose runtime `id` starts with the tags `tags[id]`
    /// and keeps its database across restarts if `persistent[id]`.
    pub fn new(tags: EnumMap<S, &'static [&'static str]>, persistent: EnumMap<S, bool>) -> Self {
        Self {
            myrmic: Myrmic::local_isolated(),
            options: CliOptions::new(tags, persistent),
        }
    }
}

impl<S: SwarmSpec> SwarmBackend<S> for Local<S> {
    type Runtime = myrmic::StoppedRt<myrmic::Local>;
    type Client = myrmic::Local;

    /// on the one shim, whichever runtime it is
    fn runtime(&self, id: S) -> myrmic::StoppedRt<myrmic::Local> {
        self.options.runtime(&self.myrmic, id)
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

impl<S: SwarmSpec> CutOff<S> for Local<S> {
    /// on a fresh [`Myrmic::cut_off`] of the one shim, whichever runtime it is
    fn cut_off_runtime(&self, id: S) -> myrmic::StoppedRt<myrmic::Local> {
        self.options.runtime(&self.myrmic.cut_off(), id)
    }
}
