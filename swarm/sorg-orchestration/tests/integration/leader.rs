use sorg_tests::set_up_log_tracker;
use test_framework::{
    World, in_process,
    overlay::Overlay,
    runtime::{Running, Stopped},
    start_runtimes,
    swarm::{self, Swarm},
};
use zenoh::config::ZenohId;

const ID_ONE: &str = "37c72f467bc9c77f41b73fe16f054741";
const ID_TWO: &str = "2cc8a35064c529faaa1924134d13e2ad";
const ID_THREE: &str = "1619e204bc90ec6fa7870dac7842dac5";

/// A swarm of three in-process nodes `a`, `b`, `c`, none started: `b` an orchestrator with the
/// db, `c` an orchestrator, and `a` whatever `a_plugins` adds. By `ZenohId`'s order, which picks
/// the leader among the orchestrators, `a < b < c`.
async fn three_nodes(
    a_plugins: fn(in_process::StoppedRt) -> in_process::StoppedRt,
) -> (Swarm, [swarm::StoppedRt<in_process::StoppedRt>; 3]) {
    let group = Overlay::new(&World::local()).new_multicast_group();
    let swarm = Swarm::new(&group).await;
    let a = swarm.add(a_plugins(in_process::StoppedRt::new(&group)).zid(zid(ID_ONE)));
    let b = swarm.add(
        in_process::StoppedRt::new(&group)
            .orchestration()
            .db()
            .zid(zid(ID_TWO)),
    );
    let c = swarm.add(
        in_process::StoppedRt::new(&group)
            .orchestration()
            .zid(zid(ID_THREE)),
    );
    (swarm, [a, b, c])
}

/// one of the test's zids
fn zid(id: &str) -> ZenohId {
    id.parse().expect("the test's zids are valid")
}

#[tokio::test(flavor = "multi_thread")]
async fn correct_leadership_handoff() -> anyhow::Result<()> {
    // Arrange - set up log tracker and start the orch with middle prio
    let tracker = set_up_log_tracker();
    let (swarm, [a, b, c]) = three_nodes(in_process::StoppedRt::orchestration).await;
    let _b = b.start().await?;

    // Assert I - when nobody is around, we are the leader
    assert!(tracker.is_leader(ID_TWO).await);

    // Act I - spin up two other orchs, one of them with a higher prio
    start_runtimes!(swarm; a, c);

    // Assert II - we should not be leader any more
    assert!(tracker.is_not_leader(ID_TWO).await);

    // Act II - remove the current leader
    let a = a.delete().await?;

    // Assert III - we should be leader again
    assert!(tracker.is_leader(ID_TWO).await);

    // Act III - remove the other orch
    let c = c.delete().await?;

    // Assert IV - we should still be leader
    assert!(tracker.is_leader(ID_TWO).await);

    // Act IV - bring the low prio orch back
    let _c = c.start().await?;

    // Assert V - we should still be leader
    assert!(tracker.is_leader(ID_TWO).await);

    // Act V - bring the high prio orch back
    let _a = a.start().await?;

    // Assert VI - we should not be leader any more
    assert!(tracker.is_not_leader(ID_TWO).await);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn non_orch_nodes_ignored_for_leader_selection() -> anyhow::Result<()> {
    // Arrange - set up log tracker and start the orch with middle prio
    let tracker = set_up_log_tracker();
    let (swarm, [a, b, c]) = three_nodes(in_process::StoppedRt::execution).await;
    let _b = b.start().await?;

    // Assert I - when nobody is around, we are the leader
    assert!(tracker.is_leader(ID_TWO).await);

    // Act I - spin up two other nodes (the exec has a "higher" ID)
    start_runtimes!(swarm; a, c);

    // Assert II - we should still be leader
    assert!(tracker.is_leader(ID_TWO).await);

    // Act II - remove the exec
    a.delete().await?;

    // Assert III - we should still be leader
    assert!(tracker.is_leader(ID_TWO).await);
    Ok(())
}
