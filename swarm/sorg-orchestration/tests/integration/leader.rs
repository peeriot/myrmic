use sorg_tests::{killable_swarm_config, set_up_log_tracker, swarm_config};

const ID_TWO: &str = "2cc8a35064c529faaa1924134d13e2ad";

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn correct_leadership_handoff() {
    // Arrange - set up log tracker and start the orch with middle prio
    let tracker = set_up_log_tracker();
    let _orch_two = swarm_config!("leader/orch_b.jsonnet");

    // Assert I - when nobody is around, we are the leader
    assert!(tracker.is_leader(ID_TWO).await);

    // // Act I - spin up two other orchs, one of them with a higher prio
    let orch_one = killable_swarm_config!("leader/orch_a.jsonnet");
    let orch_three = killable_swarm_config!("leader/orch_c.jsonnet");

    // Assert II - we should not be leader any more
    assert!(tracker.is_not_leader(ID_TWO).await);

    // Act II - remove the current leader
    drop(orch_one);

    // Assert III - we should be leader again
    assert!(tracker.is_leader(ID_TWO).await);

    // Act III - remove the other orch
    drop(orch_three);

    // Assert IV - we should still be leader
    assert!(tracker.is_leader(ID_TWO).await);

    // Act IV - bring the low prio orch back
    let _orch_three = killable_swarm_config!("leader/orch_c.jsonnet");

    // Assert V - we should still be leader
    assert!(tracker.is_leader(ID_TWO).await);

    // Act V - bring the high prio orch back
    let _orch_one = killable_swarm_config!("leader/orch_a.jsonnet");

    // Assert VI - we should not be leader any more
    assert!(tracker.is_not_leader(ID_TWO).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn non_orch_nodes_ignored_for_leader_selection() {
    // Arrange - set up log tracker and start the orch with middle prio
    let tracker = set_up_log_tracker();
    let _orch_two = swarm_config!("leader/orch_b.jsonnet");

    // Assert I - when nobody is around, we are the leader
    assert!(tracker.is_leader(ID_TWO).await);

    // Act I - spin up two other nodes (the exec has a "higher" ID)
    let orch_one = killable_swarm_config!("leader/exec_a.jsonnet");
    let _orch_three = killable_swarm_config!("leader/orch_c.jsonnet");

    // Assert II - we should still be leader
    assert!(tracker.is_leader(ID_TWO).await);

    // Act II - remove the exec
    drop(orch_one);

    // Assert III - we should still be leader
    assert!(tracker.is_leader(ID_TWO).await);
}
