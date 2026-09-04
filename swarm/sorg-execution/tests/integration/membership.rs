use sorg_tests::{enable_test_logging, killable_swarm_config, swarm_config};

use crate::integration::spawn_test_app_with_swarm;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn joining_execs_are_registered() {
    enable_test_logging("debug");

    // Arrange — two swarms, each hosting an exec with distinct tags
    let swarm_one = swarm_config!("membership/exec_one.jsonnet");
    let test_app = spawn_test_app_with_swarm(swarm_one).await;
    let _swarm_two = swarm_config!("membership/exec_two.jsonnet");

    // Act + Assert — both execs appear in the DB registry
    test_app.wait_for_registered_exec("7728").await;
    test_app.wait_for_registered_exec("a50e").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn leaving_exec_is_deregistered() {
    enable_test_logging("debug");

    // Arrange — two execs; second one is killable
    let swarm_one = swarm_config!("membership/exec_one.jsonnet");
    let test_app = spawn_test_app_with_swarm(swarm_one).await;
    let swarm_two = killable_swarm_config!("membership/exec_two.jsonnet");
    test_app.wait_for_registered_exec("7728").await;
    test_app.wait_for_registered_exec("a50e").await;

    // Act — kill the second exec
    drop(swarm_two);

    // Assert — second exec removed from registry
    test_app.wait_for_deregistered_exec("a50e").await;
}
