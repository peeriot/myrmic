//! Tests of [`test_framework::in_process`] and its swarm backend.

use std::time::Duration;

use enum_map::{Enum, EnumMap};
use test_framework::{
    assert_eventually,
    in_process::StoppedRt,
    runtime::{Running, Stopped},
    swarm2::{InProcess, Swarm},
};
use zenoh::config::ZenohId;

/// three nodes
#[derive(Clone, Copy, Debug, Enum)]
enum Node {
    A,
    B,
    C,
}

/// How long a node may take to appear to, or leave, a peer on its group.
const SEEN: Duration = Duration::from_secs(10);

/// A deleted node leaves the network: a peer on its group stops seeing it.
#[tokio::test(flavor = "multi_thread")]
async fn a_deleted_node_leaves_the_network() {
    let rt = StoppedRt::new().orchestration().start().await.unwrap();
    let zid = rt.get_zid();
    let session = zenoh::open(rt.get_zenoh_config()).await.unwrap();
    assert_eventually!(
        SEEN,
        sees(&session, zid).await,
        "the started node never showed up"
    );

    rt.delete().await.unwrap();
    assert_eventually!(
        SEEN,
        !sees(&session, zid).await,
        "the deleted node is still there"
    );
}

/// A node started again comes back under the zid it had.
#[tokio::test(flavor = "multi_thread")]
async fn a_restarted_node_keeps_its_zid() {
    let rt = StoppedRt::new().orchestration().start().await.unwrap();
    let zid = rt.get_zid();
    let session = zenoh::open(rt.get_zenoh_config()).await.unwrap();

    // kept alive until the check: dropping it would delete the node again
    let _restarted = rt.delete().await.unwrap().start().await.unwrap();
    assert_eventually!(
        SEEN,
        sees(&session, zid).await,
        "the restarted node is not there under its old zid"
    );
}

/// whether `session` has the node `zid` among its peers
async fn sees(session: &zenoh::Session, zid: ZenohId) -> bool {
    session.info().peers_zid().await.any(|peer| peer == zid)
}

/// In-process nodes form a swarm: its start checks that exactly they answer the status query, each
/// linked to the others.
#[tokio::test(flavor = "multi_thread")]
async fn in_process_nodes_form_a_swarm() {
    let nodes = EnumMap::from_fn(|_| StoppedRt::new().orchestration().db().execution());
    let (_swarm, _runtimes) = Swarm::<_, Node>::start(InProcess::new(nodes))
        .await
        .unwrap();
}
