use std::time::Duration;

use sorg_tests::{crate_path, swarm_with_config};

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_plugin_failing_before_ready_fails_startup() {
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        swarm_with_config(crate_path!("tests/data/db-bad-load-from.jsonnet")).wait_in_place(),
    )
    .await
    .expect("startup must fail fast, not run into a timeout");

    let Err(err) = result else {
        panic!("startup must fail when the db plugin cannot load its data");
    };
    let message = format!("{err:#}");
    assert!(message.contains("plugin db failed to start"), "{message}");
    assert!(message.contains("unable to read"), "{message}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_plugin_failing_after_ready_is_reported() {
    let mut spawned = swarm_with_config(crate_path!("tests/data/mqtt-port-clash.jsonnet"))
        .wait_in_place()
        .await
        .expect("the listener fails after mqtt got ready, startup must succeed");

    let err = tokio::time::timeout(Duration::from_secs(10), spawned.plugin_failure())
        .await
        .expect("the failing listener must be reported");
    spawned.kill_async().await;

    let message = format!("{err:#}");
    assert!(message.contains("plugin mqtt failed"), "{message}");
    assert!(message.contains("Address already in use"), "{message}");
}
