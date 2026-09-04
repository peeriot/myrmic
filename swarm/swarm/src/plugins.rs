use cell_protocol::node_tags::LiveTags;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fmt::Debug;
use std::sync::Arc;

use swarm_api::{DropNotifier, Ready};

use crate::config::PluginConfigs;

#[cfg(feature = "plugin-db")]
pub mod db;
#[cfg(feature = "plugin-embedded-log")]
pub mod embedded_log;
#[cfg(feature = "plugin-execution")]
pub mod execution;
#[cfg(feature = "plugin-gateway")]
pub mod gateway;
#[cfg(feature = "plugin-introspection")]
pub mod introspection;
#[cfg(feature = "plugin-mqtt")]
pub mod mqtt;
#[cfg(feature = "plugin-onboarding")]
pub mod onboarding;
#[cfg(feature = "plugin-orchestration")]
pub mod orchestration;
#[cfg(feature = "plugin-test-control")]
pub mod test_control;

/// Everything a plugin is handed besides its own configuration: the session it
/// speaks on, the runtime it spawns onto, and the signals that start and stop
/// it.
///
/// `ready` and `drop` are minted per plugin; the rest are shared handles this
/// clones cheaply.
#[derive(Clone)]
pub struct MyrmicCtx {
    session: zenoh::Session,
    handle: tokio::runtime::Handle,
    configs: Arc<PluginConfigs>,
    tags: LiveTags,
    drop: DropNotifier,
    ready: Ready,
}

impl MyrmicCtx {
    pub(crate) fn new(
        session: zenoh::Session,
        handle: tokio::runtime::Handle,
        configs: Arc<PluginConfigs>,
        tags: LiveTags,
        drop: DropNotifier,
        ready: Ready,
    ) -> Self {
        Self {
            session,
            handle,
            configs,
            tags,
            drop,
            ready,
        }
    }

    pub fn session(&self) -> &zenoh::Session {
        &self.session
    }

    pub fn handle(&self) -> &tokio::runtime::Handle {
        &self.handle
    }

    /// The configuration of every plugin on this node, for the one plugin that
    /// reports it to the network.
    pub fn configs(&self) -> &Arc<PluginConfigs> {
        &self.configs
    }

    /// This node's live tag set, shared by every plugin that acts on tags.
    pub fn tags(&self) -> &LiveTags {
        &self.tags
    }

    /// Resolves once this node is shutting down.
    pub fn drop_notifier(&self) -> DropNotifier {
        self.drop.clone()
    }

    /// The readiness signal, for plugins that hand it to the crate doing the
    /// real work. Prefer [`MyrmicCtx::notify_ready`] when signalling directly.
    pub fn ready(&self) -> Ready {
        self.ready.clone()
    }

    /// Reports this plugin as started, releasing the host's startup barrier.
    pub fn notify_ready(&self) {
        self.ready.notify_one();
    }
}

pub trait MyrmicPlugin {
    const DEFAULT_NAME: &'static str;

    type Config: Clone + Debug + DeserializeOwned + Serialize;

    fn main(
        ctx: MyrmicCtx,
        config: Self::Config,
    ) -> impl Future<Output = zenoh::Result<()>> + Send + 'static;
}
