use std::{
    future::Future,
    time::{Duration, Instant},
};

use introspection_client::v1::{Client, PluginInformation};
use zenoh::config::ZenohId;

mod membership;
mod status;

/// How long [`wait_for`] keeps re-checking before it gives up.
const WAIT_TIMEOUT: Duration = Duration::from_secs(10);

/// How often [`wait_for`] re-checks.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

async fn test_client() -> Client {
    // Join this test process's private multicast group so the client discovers
    // the swarms started via `swarm_config!` (which are isolated off the default
    // group), not a foreign process's swarm.
    let session = sorg_tests::test_session().await;
    Client::new(session).await
}

#[track_caller]
fn assert_plugin_configured(plugins: &[PluginInformation], name: &str) {
    let _plugin = plugins
        .iter()
        .find(|p_info| p_info.name == name)
        .unwrap_or_else(|| panic!("plugin not found: {}", name));
}

/// Polls `check` until it yields a value, and panics once [`WAIT_TIMEOUT`] is up.
///
/// Every assertion here observes a zenoh network that is still converging, so it
/// waits for the state it needs. Sleeping a fixed time instead only asserts that
/// the machine running the tests is fast enough.
async fn wait_for<T, F, Fut>(what: &str, check: F) -> T
where
    F: Fn() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        if let Some(found) = check().await {
            return found;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Waits until `id` shows up in `client.current_nodes()`.
///
/// A node started through `killable_swarm_config!` needs a few hundred
/// milliseconds after the macro returns before it is up and the network has seen
/// it - longer on a loaded machine.
async fn wait_for_current_node(client: &Client, id: ZenohId) {
    wait_for(
        &format!("node {id} in current_nodes()"),
        move || async move {
            let nodes = client
                .current_nodes()
                .await
                .expect("failed to query current nodes");
            nodes.iter().any(|node| node.id == id).then_some(())
        },
    )
    .await;
}
