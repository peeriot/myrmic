use std::{str::FromStr, time::Duration};

use sorg_tests::enable_test_logging;
use sorg_tests::swarm_config;
use zenoh::config::ZenohId;

use crate::integration::{assert_plugin_configured, test_client};

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn empty() {
    enable_test_logging("debug");

    // Arrange - client with test session
    let client = test_client().await;

    // Act - request swarm status
    let status = client.swarm_status().await.unwrap();

    // Assert that we don't have any swarm nodes around
    assert!(status.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn one_node() {
    enable_test_logging("debug");

    // Arrange - one swarm node and a client
    let _swarm_handle = swarm_config!("one_node.jsonnet");
    let client = test_client().await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Act - request swarm status
    let mut status = client.swarm_status().await.unwrap();

    // Assert - We expect
    // - one status in the response
    // - the correct info about the orchestration and the filestore plugin
    // - the info that it is not connected to anything else

    assert_eq!(1, status.len());
    let node_status = status.swap_remove(0);

    assert_eq!(expected_id(), node_status.id);
    assert!(node_status.peers.is_empty());
    assert!(node_status.routers.is_empty());

    // check the plugin config
    let plugins = &node_status.plugins;
    assert_eq!(3, plugins.len());

    assert_plugin_configured(plugins, "introspection");
    assert_plugin_configured(plugins, "orchestration");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn three_nodes() {
    enable_test_logging("debug");

    // Arrange - one swarm node and a client
    let _swarm_handle_one = swarm_config!("three_nodes_a.jsonnet");
    let _swarm_handle_two = swarm_config!("three_nodes_b.jsonnet");
    let _swarm_handle_three = swarm_config!("three_nodes_c.jsonnet");
    let client = test_client().await;

    // Act - request swarm status
    let status = client.swarm_status().await.unwrap();

    // Assert
    assert_eq!(3, status.len());
    for node_stat in status {
        if node_stat.id == expected_id() {
            assert_eq!(1, node_stat.peers.len());
            assert!(node_stat.peers.contains(&expected_id_other_peer()));
            assert_eq!(1, node_stat.routers.len());
            assert!(node_stat.routers.contains(&expected_id_router()));

            // check the plugin config
            let plugins = &node_stat.plugins;
            assert_eq!(3, plugins.len());

            assert_plugin_configured(plugins, "introspection");
            assert_plugin_configured(plugins, "orchestration");
        } else if node_stat.id == expected_id_router() {
            assert_eq!(2, node_stat.peers.len());
            assert!(node_stat.peers.contains(&expected_id_other_peer()));
            assert!(node_stat.peers.contains(&expected_id()));

            // check the plugin configa
            let plugins = &node_stat.plugins;
            assert_eq!(3, plugins.len());
            assert_plugin_configured(plugins, "introspection");
            assert_plugin_configured(plugins, "orchestration");
            assert_plugin_configured(plugins, "execution");
        } else if node_stat.id == expected_id_other_peer() {
            assert_eq!(1, node_stat.peers.len());
            assert!(node_stat.peers.contains(&expected_id()));
            assert_eq!(1, node_stat.routers.len());
            assert!(node_stat.routers.contains(&expected_id_router()));

            // check the plugin config
            let plugins = &node_stat.plugins;
            assert_eq!(2, plugins.len());
            assert_plugin_configured(plugins, "introspection");
            assert_plugin_configured(plugins, "execution");
        } else {
            panic!("unexpected node id")
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn own_status() {
    enable_test_logging("debug");

    // Arrange - one swarm node; client shares the node's session
    let swarm_handle = swarm_config!("one_node.jsonnet");
    let session = swarm_handle.session();
    let client = introspection_client::v1::Client::new(session.clone()).await;

    // Act - request own node status
    let status = client.own_status().await.unwrap();

    // Assert - we get back our own node with the expected plugins
    assert_eq!(expected_id(), status.id);
    assert!(status.peers.is_empty());
    assert!(status.routers.is_empty());

    let plugins = &status.plugins;
    assert_eq!(3, plugins.len());
    assert_plugin_configured(plugins, "introspection");
    assert_plugin_configured(plugins, "orchestration");
    assert_plugin_configured(plugins, "db");
}

fn expected_id() -> ZenohId {
    ZenohId::from_str("9a28d1a01a04f41e7b9e0ff3bab594a2").unwrap()
}

fn expected_id_router() -> ZenohId {
    ZenohId::from_str("8a28d1a01a04f41e7b9e0ff3bab594a2").unwrap()
}
fn expected_id_other_peer() -> ZenohId {
    ZenohId::from_str("7a28d1a01a04f41e7b9e0ff3bab594a2").unwrap()
}
