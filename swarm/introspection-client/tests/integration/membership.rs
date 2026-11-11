use std::{collections::HashMap, sync::Arc, time::Duration};

use claims::assert_ok;
use introspection_client::v1::Client;
use introspection_common::v1::NodeStatus;
use sorg_tests::enable_test_logging;
use test_framework::{
    in_process,
    myrmic::{self, Myrmic},
    runtime::{Kill, Running, Stopped},
    wait::DEFAULT_TIMEOUT,
    wait_for,
};
use tokio::sync::Mutex;
use zenoh::config::ZenohId;

use crate::integration::{assert_plugin_configured, wait_for_current_node};

#[tokio::test(flavor = "multi_thread")]
async fn membership_callbacks_are_called() {
    enable_test_logging("debug");
    let myrmic = Myrmic::local_isolated();
    let observer = observer(&myrmic).await;

    // Arrange - we have a shared state monitoring the statuses of member nodes
    let known_nodes: Arc<Mutex<HashMap<ZenohId, NodeStatus>>> =
        Arc::new(Mutex::new(HashMap::default()));
    let client = Client::new(observer.get_session().clone()).await;

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

    // Act I - a node joins
    let joiner = myrmic.runtime(&[]).start().await.unwrap();
    let joiner_zid = joiner.get_zid();

    // Assert I - verify that we now have the joining node in our state
    let status = wait_for(DEFAULT_TIMEOUT, "the joining node to reach on_join", || {
        let known_nodes = known_nodes.clone();
        async move { known_nodes.lock().await.get(&joiner_zid).cloned() }
    })
    .await;
    let plugins = status.plugins;
    assert_plugin_configured(&plugins, "introspection");
    assert_plugin_configured(&plugins, "orchestration");

    // Act II - the node dies
    joiner.kill().await.unwrap();

    // Assert II - verify that we no longer have the node in our state
    wait_for(
        DEFAULT_TIMEOUT,
        "the leaving node to reach on_leave",
        || {
            let known_nodes = known_nodes.clone();
            async move {
                known_nodes
                    .lock()
                    .await
                    .get(&joiner_zid)
                    .is_none()
                    .then_some(())
            }
        },
    )
    .await;
}

/// Verifies that `current_nodes()` returns a node that was already in the network
/// before our node started — the case that motivated this API.
#[tokio::test(flavor = "multi_thread")]
async fn current_nodes_finds_pre_existing_node() {
    enable_test_logging("debug");
    let myrmic = Myrmic::local_isolated();

    // Arrange - the other node is already running before our node starts
    let pre_existing = myrmic.runtime(&[]).start().await.unwrap();
    let observer = observer(&myrmic).await;

    let client = Client::new(observer.get_session().clone()).await;

    // Act + Assert - we see the node that was there before us
    wait_for_current_node(&client, pre_existing.get_zid()).await;
}

/// Verifies that `current_nodes()` also returns a node that joined after us.
#[tokio::test(flavor = "multi_thread")]
async fn current_nodes_finds_later_joining_node() {
    enable_test_logging("debug");
    let myrmic = Myrmic::local_isolated();

    // Arrange - our node is up first, then another node joins
    let observer = observer(&myrmic).await;
    let later = myrmic.runtime(&[]).start().await.unwrap();

    let client = Client::new(observer.get_session().clone()).await;

    // Act + Assert - we eventually see the node that joined after us
    wait_for_current_node(&client, later.get_zid()).await;
}

/// Verifies that `current_nodes()` provides catch-up for already-present nodes
/// while `on_join` fires for nodes that join afterwards.
#[tokio::test(flavor = "multi_thread")]
async fn current_nodes_and_on_join_combined() {
    enable_test_logging("debug");
    let myrmic = Myrmic::local_isolated();

    // Arrange - one node is already running before our node starts
    let pre_existing = myrmic.runtime(&[]).start().await.unwrap();
    let observer = observer(&myrmic).await;

    let client = Client::new(observer.get_session().clone()).await;

    // Act I + Assert I - current nodes catches the pre-existing node up
    wait_for_current_node(&client, pre_existing.get_zid()).await;

    // Arrange II - register on_join for future events
    let joined_nodes: Arc<Mutex<Vec<NodeStatus>>> = Arc::new(Mutex::new(vec![]));
    assert_ok!(
        client
            .on_join(joined_nodes.clone(), |node_status, joined| async move {
                joined.lock().await.push(node_status);
            })
            .await
    );
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Act II - a third node joins after on_join is registered
    let new_node = myrmic.runtime(&[]).start().await.unwrap();
    let new_zid = new_node.get_zid();

    // Assert II - on_join fired for the new node
    wait_for(DEFAULT_TIMEOUT, "the third node to reach on_join", || {
        let joined_nodes = joined_nodes.clone();
        async move {
            joined_nodes
                .lock()
                .await
                .iter()
                .find(|node| node.id == new_zid)
                .cloned()
        }
    })
    .await;
}

/// The node the test's client runs on, in this process, on `myrmic`'s multicast group. The
/// client's callbacks and `current_nodes()` only work on the session of a node with an
/// introspection plugin; the other nodes are runtimes of `myrmic`.
async fn observer(myrmic: &Myrmic<myrmic::Local>) -> in_process::RunningRt {
    in_process::StoppedRt::new()
        .orchestration()
        .db()
        .multicast_group(myrmic.backend().get_multicast_group())
        .start()
        .await
        .expect("the observing node failed to start")
}
