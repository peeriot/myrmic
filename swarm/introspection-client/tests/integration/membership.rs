use std::{
    collections::HashMap,
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};

use claims::{assert_none, assert_ok, assert_some};
use introspection_client::v1::Client;
use introspection_common::v1::NodeStatus;
use sorg_tests::{enable_test_logging, killable_swarm_config, swarm_config};
use tokio::sync::Mutex;
use zenoh::config::ZenohId;

use crate::integration::assert_plugin_configured;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn membership_callbacks_are_called() {
    enable_test_logging("debug");
    let node = swarm_config!("other_node.jsonnet");
    let session = node.session();

    // Arrange - we have a shared state monitoring the statuses of member nodes
    let known_nodes: Arc<Mutex<HashMap<ZenohId, NodeStatus>>> =
        Arc::new(Mutex::new(HashMap::default()));
    let client = Client::new(session.clone()).await;

    // Register join callback
    assert_ok!(
        client
            .on_join(known_nodes.clone(), |node_status, known_nodes| async move {
                let id = node_status.id;
                known_nodes.lock().await.insert(id, node_status.clone());
            })
            .await
    );

    // Register leave callback
    assert_ok!(
        client
            .on_leave(known_nodes.clone(), |node_id, known_nodes| async move {
                known_nodes.lock().await.remove(&node_id);
            })
            .await
    );
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Act I - simulate a node joining
    let node_handle = killable_swarm_config!("one_node.jsonnet");
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Assert I - verify that we now have the expected node in our state
    let status = assert_some!(known_nodes.lock().await.get(&expected_id()).cloned());
    let plugins = status.plugins;
    assert_plugin_configured(&plugins, "introspection");
    assert_plugin_configured(&plugins, "orchestration");

    // Act II - simulate a node leaving
    let _other_node_handle = killable_swarm_config!("other_node.jsonnet");
    tokio::time::sleep(Duration::from_millis(100)).await;

    drop(node_handle);
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Assert II - verify that we no longer have the expected node in our state
    assert_none!(known_nodes.lock().await.get(&expected_id()));
}

/// Verifies that `current_nodes()` returns a node that was already in the network
/// before our node started — the case that motivated this API.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn current_nodes_finds_pre_existing_node() {
    enable_test_logging("debug");

    // Arrange - the other node is already running before our node starts
    let _pre_existing = killable_swarm_config!("one_node.jsonnet");
    let node = swarm_config!("other_node.jsonnet");

    let client = Client::new(node.session().clone()).await;

    // Act
    let nodes = client.current_nodes().await.unwrap();

    // Assert - we see the node that was there before us
    assert_some!(nodes.iter().find(|ns| ns.id == expected_id()));
}

/// Verifies that `current_nodes()` also returns a node that joined after us.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn current_nodes_finds_later_joining_node() {
    enable_test_logging("debug");

    // Arrange - our node is up first, then another node joins
    let node = swarm_config!("other_node.jsonnet");
    let _later_node = killable_swarm_config!("one_node.jsonnet");

    let client = Client::new(node.session().clone()).await;

    // Assert - we eventually see the node that joined after us. Poll rather than
    // sleep a fixed time: the later node is a freshly-spawned subprocess swarm,
    // which takes a few hundred ms to start and be discovered (longer under load).
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let nodes = client.current_nodes().await.unwrap();
        if nodes.iter().any(|ns| ns.id == expected_id()) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "later-joining node {} never appeared in current_nodes()",
            expected_id()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Verifies that `current_nodes()` provides catch-up for already-present nodes
/// while `on_join` fires for nodes that join afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn current_nodes_and_on_join_combined() {
    enable_test_logging("debug");

    // Arrange - one node is already running before our node starts
    let _pre_existing = killable_swarm_config!("one_node.jsonnet");
    let node = swarm_config!("other_node.jsonnet");

    let session = node.session().clone();
    let client = Client::new(session).await;

    // Act I - get current nodes (catch-up for the pre-existing node)
    let nodes = client.current_nodes().await.unwrap();
    assert_some!(nodes.iter().find(|ns| ns.id == expected_id()));

    // Arrange II - register on_join for future events
    let joined_nodes: Arc<Mutex<Vec<NodeStatus>>> = Arc::new(Mutex::new(Vec::new()));
    assert_ok!(
        client
            .on_join(joined_nodes.clone(), |node_status, joined| async move {
                joined.lock().await.push(node_status);
            })
            .await
    );
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Act II - a third node joins after on_join is registered
    let _new_node = killable_swarm_config!("third_node.jsonnet");
    // 500ms: external processes take longer to announce under full-suite load
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Assert II - on_join fired for the new node
    let joined = joined_nodes.lock().await;
    let third_node_id = expected_id_third();
    assert_some!(joined.iter().find(|ns| ns.id == third_node_id));
}

fn expected_id() -> ZenohId {
    ZenohId::from_str("9a28d1a01a04f41e7b9e0ff3bab594a2").unwrap()
}

fn expected_id_third() -> ZenohId {
    ZenohId::from_str("7a28d1a01a04f41e7b9e0ff3bab594a2").unwrap()
}
