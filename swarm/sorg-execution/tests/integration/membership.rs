use cell_protocol::RuntimeId;
use sorg_tests::enable_test_logging;
use test_framework::{
    World, assert_eventually, in_process, overlay::Overlay, runtime::Running, swarm3::Swarm,
    wait::DEFAULT_TIMEOUT,
};

#[tokio::test(flavor = "multi_thread")]
async fn joining_execs_are_registered() -> anyhow::Result<()> {
    enable_test_logging("debug");

    // Arrange — a runtime, and a second one joining its swarm
    let group = Overlay::new(&World::local()).new_multicast_group();
    let swarm = Swarm::new(&group).await;
    let start = || {
        swarm
            .add(swarm.get_myrmic().runtime())
            .start_without_check()
    };
    let one = start().await?;
    let two = start().await?;

    // Act + Assert — both execs appear in the DB registry
    for runtime in [&one, &two] {
        let id = RuntimeId::from(runtime.get_zid());
        assert_eventually!(
            DEFAULT_TIMEOUT,
            swarm
                .get_sorg()
                .list_registered_execs()
                .await
                .is_ok_and(|execs| execs.iter().any(|exec| exec.id() == id)),
            "exec {id} never showed up in the registry"
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn leaving_exec_is_deregistered() -> anyhow::Result<()> {
    enable_test_logging("debug");

    // Arrange — two execs, registered once their start returns (the exec plugin reports ready
    // only after registering), so the swarm's connectivity check would add nothing but time. On
    // CLI runtimes, which all run a db, the second's row might not have reached the first's
    // replica before the second goes, and then it would be missing without anyone having removed
    // it. So only `one` runs a db: `two`'s registry row can then only live on `one`, so it is gone
    // from there only when someone removes it.
    let group = Overlay::new(&World::local()).new_multicast_group();
    let swarm = Swarm::new(&group).await;
    let _one = swarm
        .add(in_process::StoppedRt::new(&group).execution().db())
        .start_without_check()
        .await?;
    let two = swarm
        .add(in_process::StoppedRt::new(&group).execution())
        .start_without_check()
        .await?;
    let id = RuntimeId::from(two.get_zid());

    // Act — the second exec leaves
    two.delete().await?;

    // Assert — second exec removed from registry
    assert_eventually!(
        DEFAULT_TIMEOUT,
        swarm
            .get_sorg()
            .list_registered_execs()
            .await
            .is_ok_and(|execs| execs.iter().all(|exec| exec.id() != id)),
        "exec {id} is still in the registry"
    );
    Ok(())
}
