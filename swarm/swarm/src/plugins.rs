use cell_protocol::node_tags::LiveTags;
use futures_util::future::select_all;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fmt::{Debug, Display};
use std::sync::Arc;
use tokio::task::JoinHandle;

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

/// Deserializes an optional humantime duration string (e.g. `"100ms"`, `"30s"`, `"1min"`).
#[cfg(any(feature = "plugin-db", feature = "plugin-onboarding"))]
pub(crate) fn deserialize_optional_duration<'de, D>(
    deserializer: D,
) -> Result<Option<std::time::Duration>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s: Option<String> = serde::Deserialize::deserialize(deserializer)?;
    match s {
        None => Ok(None),
        Some(s) => humantime::parse_duration(&s)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

/// Waits for the first of `tasks` to end and says how it ended. Never resolves
/// for an empty list. Cancel-safe, but must not be polled again once it
/// resolved: the finished handle would be polled twice.
pub(crate) async fn first_ended<N>(tasks: &mut [(N, JoinHandle<zenoh::Result<()>>)]) -> String
where
    N: Display,
{
    if tasks.is_empty() {
        return std::future::pending().await;
    }
    let (result, index, _) = select_all(tasks.iter_mut().map(|(_, task)| task)).await;
    let name = &tasks[index].0;

    match result {
        Ok(Ok(())) => format!("{name} stopped"),
        Ok(Err(err)) => format!("{name} failed: {err}"),
        Err(err) => format!("{name} failed: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::task::JoinHandle;

    use super::first_ended;

    #[tokio::test]
    async fn ok_end_is_stopped() {
        let mut tasks = [("x", pending_task()), ("y", task(async { Ok(()) }))];
        assert_eq!(first_ended(&mut tasks).await, "y stopped");
    }

    #[tokio::test]
    async fn err_end_is_failed() {
        let mut tasks = [("x", task(async { Err("boom".into()) }))];
        assert_eq!(first_ended(&mut tasks).await, "x failed: boom");
    }

    #[tokio::test]
    async fn panic_end_is_failed() {
        let mut tasks = [("x", task(async { panic!("boom") }))];
        let message = first_ended(&mut tasks).await;
        assert!(message.contains("x failed"), "{message}");
        assert!(message.contains("boom"), "{message}");
    }

    #[tokio::test]
    async fn running_tasks_stay_pending() {
        let mut tasks = [("x", pending_task())];
        assert!(stays_pending(&mut tasks).await);
    }

    #[tokio::test]
    async fn no_tasks_stay_pending() {
        let mut tasks: [(&str, JoinHandle<zenoh::Result<()>>); 0] = [];
        assert!(stays_pending(&mut tasks).await);
    }

    #[tokio::test]
    async fn dropped_wait_keeps_watching() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let mut tasks = [(
            "x",
            task(async move {
                let _ = rx.await;
                Err("boom".into())
            }),
        )];

        assert!(stays_pending(&mut tasks).await);
        tx.send(()).expect("task still waiting");
        assert_eq!(first_ended(&mut tasks).await, "x failed: boom");
    }

    fn task<F>(fut: F) -> JoinHandle<zenoh::Result<()>>
    where
        F: Future<Output = zenoh::Result<()>> + Send + 'static,
    {
        tokio::spawn(fut)
    }

    fn pending_task() -> JoinHandle<zenoh::Result<()>> {
        task(async {
            std::future::pending::<()>().await;
            Ok(())
        })
    }

    async fn stays_pending(tasks: &mut [(&str, JoinHandle<zenoh::Result<()>>)]) -> bool {
        tokio::time::timeout(Duration::from_millis(100), first_ended(tasks))
            .await
            .is_err()
    }
}
