use introspection_client::v1::Client;
use sorg_tests::enable_test_logging;
use test_framework::{
    in_process,
    myrmic::Myrmic,
    runtime::{Running, Stopped},
};

use crate::integration::{assert_plugin_configured, wait_for_swarm_status};

#[tokio::test(flavor = "multi_thread")]
async fn empty() {
    enable_test_logging("debug");

    // Arrange - client with a session on a multicast group nothing else is on
    let session = zenoh::open(Myrmic::local_isolated().backend().zenoh_config())
        .await
        .unwrap();
    let client = Client::new(session).await;

    // Act - request swarm status
    let status = client.swarm_status().await.unwrap();

    // Assert that we don't have any swarm nodes around
    assert!(status.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn one_node() {
    enable_test_logging("debug");

    // Arrange - one swarm node and a client
    let node = in_process::StoppedRt::new()
        .orchestration()
        .db()
        .start()
        .await
        .unwrap();
    let client = Client::new(zenoh::open(node.get_zenoh_config()).await.unwrap()).await;

    // Act - request swarm status
    let mut status = wait_for_swarm_status(&client, 1).await;

    // Assert - We expect
    // - one status in the response
    // - the correct info about the orchestration and the filestore plugin
    // - the info that it is not connected to anything else

    assert_eq!(1, status.len());
    let node_status = status.swap_remove(0);

    assert_eq!(node.get_zid(), node_status.id);
    assert!(node_status.peers.is_empty());
    assert!(node_status.routers.is_empty());

    // check the plugin config
    let plugins = &node_status.plugins;
    assert_eq!(3, plugins.len());

    assert_plugin_configured(plugins, "introspection");
    assert_plugin_configured(plugins, "orchestration");
}

#[tokio::test(flavor = "multi_thread")]
async fn three_nodes() {
    enable_test_logging("debug");

    // Arrange - two peers and a router on one group, and a client. Started one by one, not as a
    // swarm: a swarm's connectivity check wants every runtime among the others' peers, and a
    // router is not.
    let peer = in_process::StoppedRt::new().orchestration().db();
    let group = peer.get_multicast_group().to_owned();
    let peer = peer.start().await.unwrap();
    let router = in_process::StoppedRt::new()
        .router()
        .execution()
        .orchestration()
        .multicast_group(&group)
        .start()
        .await
        .unwrap();
    let other_peer = in_process::StoppedRt::new()
        .execution()
        .multicast_group(&group)
        .start()
        .await
        .unwrap();
    let session = zenoh::open(peer.get_zenoh_config()).await.unwrap();
    let client = Client::new(session).await;
    let (peer_zid, router_zid, other_peer_zid) =
        (peer.get_zid(), router.get_zid(), other_peer.get_zid());

    // Act - request swarm status
    let status = wait_for_swarm_status(&client, 3).await;

    // Assert
    assert_eq!(3, status.len());
    for node_stat in status {
        if node_stat.id == peer_zid {
            assert_eq!(1, node_stat.peers.len());
            assert!(node_stat.peers.contains(&other_peer_zid));
            assert_eq!(1, node_stat.routers.len());
            assert!(node_stat.routers.contains(&router_zid));

            // check the plugin config
            let plugins = &node_stat.plugins;
            assert_eq!(3, plugins.len());

            assert_plugin_configured(plugins, "introspection");
            assert_plugin_configured(plugins, "orchestration");
        } else if node_stat.id == router_zid {
            assert_eq!(2, node_stat.peers.len());
            assert!(node_stat.peers.contains(&other_peer_zid));
            assert!(node_stat.peers.contains(&peer_zid));

            // check the plugin configa
            let plugins = &node_stat.plugins;
            assert_eq!(3, plugins.len());
            assert_plugin_configured(plugins, "introspection");
            assert_plugin_configured(plugins, "orchestration");
            assert_plugin_configured(plugins, "execution");
        } else if node_stat.id == other_peer_zid {
            assert_eq!(1, node_stat.peers.len());
            assert!(node_stat.peers.contains(&peer_zid));
            assert_eq!(1, node_stat.routers.len());
            assert!(node_stat.routers.contains(&router_zid));

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

#[tokio::test(flavor = "multi_thread")]
async fn own_status() {
    enable_test_logging("debug");

    // Arrange - one swarm node; client shares the node's session
    let node = in_process::StoppedRt::new()
        .orchestration()
        .db()
        .start()
        .await
        .unwrap();
    let client = Client::new(node.get_session().clone()).await;

    // Act - request own node status
    let status = client.own_status().await.unwrap();

    // Assert - we get back our own node with the expected plugins
    assert_eq!(node.get_zid(), status.id);
    assert!(status.peers.is_empty());
    assert!(status.routers.is_empty());

    let plugins = &status.plugins;
    assert_eq!(3, plugins.len());
    assert_plugin_configured(plugins, "introspection");
    assert_plugin_configured(plugins, "orchestration");
    assert_plugin_configured(plugins, "db");
}
