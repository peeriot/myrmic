//! Tests of [`test_framework::in_process`], alone and as a swarm.

use std::time::Duration;

use test_framework::{
    World, assert_eventually,
    in_process::StoppedRt,
    overlay::Overlay,
    runtime::{Running, Stopped},
    start_runtimes,
    swarm::Swarm,
};
use zenoh::config::ZenohId;

/// How long a node may take to appear to, or leave, a peer on its group.
const SEEN: Duration = Duration::from_secs(10);

/// A deleted node leaves the network: a peer on its group stops seeing it.
#[tokio::test(flavor = "multi_thread")]
async fn a_deleted_node_leaves_the_network() -> anyhow::Result<()> {
    let group = Overlay::new(&World::local()).new_multicast_group();
    let rt = StoppedRt::new(&group).orchestration().start().await?;
    let zid = rt.get_zid();
    let session = zenoh::open(group.get_zenoh_config()).await.unwrap();
    assert_eventually!(
        SEEN,
        sees(&session, zid).await,
        "the started node never showed up"
    );

    rt.delete().await?;
    assert_eventually!(
        SEEN,
        !sees(&session, zid).await,
        "the deleted node is still there"
    );
    Ok(())
}

/// A node started again comes back under the zid it had.
#[tokio::test(flavor = "multi_thread")]
async fn a_restarted_node_keeps_its_zid() -> anyhow::Result<()> {
    let group = Overlay::new(&World::local()).new_multicast_group();
    let rt = StoppedRt::new(&group).orchestration().start().await?;
    let zid = rt.get_zid();
    let session = zenoh::open(group.get_zenoh_config()).await.unwrap();

    // kept alive until the check: dropping it would delete the node again
    let _restarted = rt.delete().await?.start().await?;
    assert_eventually!(
        SEEN,
        sees(&session, zid).await,
        "the restarted node is not there under its old zid"
    );
    Ok(())
}

/// whether `session` has the node `zid` among its peers
async fn sees(session: &zenoh::Session, zid: ZenohId) -> bool {
    session.info().peers_zid().await.any(|peer| peer == zid)
}

/// In-process nodes form a swarm: its start checks that exactly they answer the status query, each
/// linked to the others.
#[tokio::test(flavor = "multi_thread")]
async fn in_process_nodes_form_a_swarm() -> anyhow::Result<()> {
    let group = Overlay::new(&World::local()).new_multicast_group();
    let swarm = Swarm::new(&group).await;
    let node = || swarm.add(StoppedRt::new(&group).orchestration().db().execution());
    let (a, b, c) = (node(), node(), node());
    start_runtimes!(swarm; a, b, c);
    Ok(())
}

/// A node in zenoh's router mode is linked like a peer: a swarm with one connects.
#[tokio::test(flavor = "multi_thread")]
async fn a_router_node_joins_a_swarm() -> anyhow::Result<()> {
    let group = Overlay::new(&World::local()).new_multicast_group();
    let swarm = Swarm::new(&group).await;
    let peer = swarm.add(StoppedRt::new(&group).orchestration().db());
    let router = swarm.add(StoppedRt::new(&group).router_mode().execution());
    start_runtimes!(swarm; peer, router);
    Ok(())
}
