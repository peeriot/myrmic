use std::{collections::HashMap, sync::Arc, time::Duration};

use introspection_client::v1::Client;
use introspection_common::v1::NodeStatus;
use sorg_tests::enable_test_logging;
use test_framework::{
    in_process, myrmic,
    overlay::Overlay,
    runtime::Running,
    swarm3::{self, Swarm},
    wait::{DEFAULT_TIMEOUT, Timeout},
    wait_for,
};
use tokio::sync::Mutex;
use zenoh::config::ZenohId;

use crate::integration::{assert_plugin_configured, wait_for_current_node};

#[tokio::test(flavor = "multi_thread")]
async fn membership_callbacks_are_called() -> anyhow::Result<()> {
    let setup = ObservedSwarm::new().await?;
    let members = track_members(&setup.client).await?;

    // Act I - a node joins
    let joiner = setup.start_runtime().await?;
    let zid = joiner.get_zid();

    // Assert I - the joining node reached on_join, with its status
    let status = wait_until_member(&members, zid).await?;
    assert_plugin_configured(&status.plugins, "introspection");
    assert_plugin_configured(&status.plugins, "orchestration");

    // Act II - the node dies
    joiner.stop_with(|rt| rt.kill()).await?;

    // Assert II - the leaving node reached on_leave
    wait_until_gone(&members, zid).await?;
    Ok(())
}

/// Verifies that `current_nodes()` returns a node that was already in the network
/// before our node started — the case that motivated this API.
#[tokio::test(flavor = "multi_thread")]
async fn current_nodes_finds_pre_existing_node() -> anyhow::Result<()> {
    let setup = ObservedSwarm::new().await?;

    wait_for_current_node(&setup.client, setup.pre_existing.get_zid()).await?;
    Ok(())
}

/// Verifies that `current_nodes()` also returns a node that joined after us.
#[tokio::test(flavor = "multi_thread")]
async fn current_nodes_finds_later_joining_node() -> anyhow::Result<()> {
    let setup = ObservedSwarm::new().await?;

    let later = setup.start_runtime().await?;

    wait_for_current_node(&setup.client, later.get_zid()).await?;
    Ok(())
}

/// Verifies that `current_nodes()` provides catch-up for already-present nodes
/// while `on_join` fires for nodes that join afterwards.
#[tokio::test(flavor = "multi_thread")]
async fn current_nodes_and_on_join_combined() -> anyhow::Result<()> {
    let setup = ObservedSwarm::new().await?;

    // Act I + Assert I - current nodes catches the pre-existing node up
    wait_for_current_node(&setup.client, setup.pre_existing.get_zid()).await?;

    // Act II - a third node joins after on_join is registered
    let members = track_members(&setup.client).await?;
    let new_node = setup.start_runtime().await?;

    // Assert II - on_join fired for the new node
    wait_until_member(&members, new_node.get_zid()).await?;
    Ok(())
}

/// a running CLI runtime of the swarm
type CliRt = swarm3::RunningRt<myrmic::RunningRt<myrmic::Local>>;

/// A swarm of a CLI runtime and the observer, an in-process node the test's client runs on: the
/// client's callbacks and `current_nodes()` only work on the session of a node with an
/// introspection plugin. The CLI runtime starts first, so it was there before the observer.
///
/// Nothing waits for the swarm's connectivity check: every test waits for what it observes
/// itself (`current_nodes()`, the callbacks), and the check's settle time would only add to it.
struct ObservedSwarm {
    swarm: Swarm,
    pre_existing: CliRt,
    /// only kept alive: the client runs on its session
    _observer: swarm3::RunningRt<in_process::RunningRt>,
    client: Client,
}

impl ObservedSwarm {
    async fn new() -> anyhow::Result<Self> {
        enable_test_logging("debug");
        let group = Overlay::new().new_multicast_group();
        let swarm = Swarm::new(&group).await;
        let pre_existing = swarm
            .add(swarm.get_client().runtime())
            .start_without_check()
            .await?;
        let observer = swarm
            .add(in_process::StoppedRt::new(&group).orchestration().db())
            .start_without_check()
            .await?;
        let client = Client::new(observer.get_session().clone()).await;
        Ok(Self {
            swarm,
            pre_existing,
            _observer: observer,
            client,
        })
    }

    /// start another CLI runtime in the swarm, without the connectivity check
    async fn start_runtime(&self) -> Result<CliRt, myrmic::Error> {
        self.swarm
            .add(self.swarm.get_client().runtime())
            .start_without_check()
            .await
    }
}

/// the nodes a client's callbacks report as joined and not left yet, by zid
type Members = Arc<Mutex<HashMap<ZenohId, NodeStatus>>>;

/// The nodes that join (`on_join`) after this call and haven't left (`on_leave`) yet, as
/// `client` reports them.
async fn track_members(client: &Client) -> anyhow::Result<Members> {
    let members = Members::default();
    client
        .on_join(members.clone(), |status, members| async move {
            members.lock().await.insert(status.id, status);
        })
        .await?;
    client
        .on_leave(members.clone(), |zid, members| async move {
            members.lock().await.remove(&zid);
        })
        .await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    Ok(members)
}

/// Wait until the node `zid` is among `members`; returns its status from `on_join`.
async fn wait_until_member(members: &Members, zid: ZenohId) -> Result<NodeStatus, Timeout> {
    wait_for(
        DEFAULT_TIMEOUT,
        &format!("node {zid} to reach on_join"),
        move || async move { members.lock().await.get(&zid).cloned() },
    )
    .await
}

/// Wait until the node `zid` is no longer among `members`.
async fn wait_until_gone(members: &Members, zid: ZenohId) -> Result<(), Timeout> {
    wait_for(
        DEFAULT_TIMEOUT,
        &format!("node {zid} to reach on_leave"),
        move || async move { (!members.lock().await.contains_key(&zid)).then_some(()) },
    )
    .await
}
