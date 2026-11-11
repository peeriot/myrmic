use cell_protocol::RuntimeId;
use enum_map::{Enum, EnumMap};
use sorg_tests::enable_test_logging;
use test_framework::{
    assert_eventually, in_process,
    runtime::{Running, Stopped},
    swarm2::{InProcess, Local, Swarm},
    wait::DEFAULT_TIMEOUT,
};

/// two runtimes, the second joining the swarm of the first
#[derive(Clone, Copy, Debug, Enum)]
enum Node {
    One,
    Two,
}

/// a local swarm of [`Node`], without tags or persistence
fn local() -> Local<Node> {
    Local::new(
        EnumMap::from_fn(|_| -> &[&str] { &[] }),
        EnumMap::from_fn(|_| false),
    )
}

/// An in-process swarm of [`Node`] in which only `One` runs a db: `Two`'s registry row can then
/// only live on `One`, so it is gone from there only when someone removes it.
fn in_process() -> InProcess<Node> {
    InProcess::new(EnumMap::from_fn(|id| match id {
        Node::One => in_process::StoppedRt::new().execution().db(),
        Node::Two => in_process::StoppedRt::new().execution(),
    }))
}

#[tokio::test(flavor = "multi_thread")]
async fn joining_execs_are_registered() {
    enable_test_logging("debug");

    // Arrange — a runtime, and a second one joining its swarm
    let (swarm, [one, two]) = Swarm::new(local()).await;
    let _one = one.start().await.unwrap();
    let _two = two.start().await.unwrap();

    // Act + Assert — both execs appear in the DB registry
    for (_, entry) in swarm.get_entries() {
        let id = RuntimeId::from(entry.get_zid());
        assert_eventually!(
            DEFAULT_TIMEOUT,
            swarm
                .sorg()
                .list_registered_execs()
                .await
                .is_ok_and(|execs| execs.iter().any(|exec| exec.id() == id)),
            "exec {id} never showed up in the registry"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn leaving_exec_is_deregistered() {
    enable_test_logging("debug");

    // Arrange — two execs, registered once their start returns. On CLI runtimes, which all run a
    // db, the second's row might not have reached the first's replica before the second goes,
    // and then it would be missing without anyone having removed it.
    let (swarm, [_one, two]) = Swarm::start(in_process()).await.unwrap();
    let id = RuntimeId::from(two.get_zid());

    // Act — the second exec leaves
    two.delete().await.unwrap();

    // Assert — second exec removed from registry
    assert_eventually!(
        DEFAULT_TIMEOUT,
        swarm
            .sorg()
            .list_registered_execs()
            .await
            .is_ok_and(|execs| execs.iter().all(|exec| exec.id() != id)),
        "exec {id} is still in the registry"
    );
}
