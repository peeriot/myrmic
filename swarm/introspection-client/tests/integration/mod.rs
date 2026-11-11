use introspection_client::v1::{Client, PluginInformation};
use introspection_common::v1::NodeStatus;
use test_framework::{wait::DEFAULT_TIMEOUT, wait_for};
use zenoh::config::ZenohId;

mod membership;
mod status;

#[track_caller]
fn assert_plugin_configured(plugins: &[PluginInformation], name: &str) {
    let _plugin = plugins
        .iter()
        .find(|p_info| p_info.name == name)
        .unwrap_or_else(|| panic!("plugin not found: {}", name));
}

/// Waits until `client.swarm_status()` reports at least `count` nodes and returns
/// that status.
///
/// A session that was just opened has not necessarily discovered every node yet,
/// and the query only reaches the nodes it already knows about, so the first
/// answer can come back short. The caller still asserts the exact count it wants.
async fn wait_for_swarm_status(client: &Client, count: usize) -> Vec<NodeStatus> {
    wait_for(
        DEFAULT_TIMEOUT,
        &format!("{count} nodes in swarm_status()"),
        move || async move {
            let status = client
                .swarm_status()
                .await
                .expect("failed to query swarm status");
            (status.len() >= count).then_some(status)
        },
    )
    .await
}

/// Waits until `id` shows up in `client.current_nodes()`.
///
/// A runtime is ready when its start returns, but the observing node only lists
/// it once its introspection plugin has seen it join - a moment later, longer on
/// a loaded machine.
async fn wait_for_current_node(client: &Client, id: ZenohId) {
    wait_for(
        DEFAULT_TIMEOUT,
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
