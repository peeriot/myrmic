//! A trap or panic in a handler is a crash: the incarnation ends, a root's
//! restart policy decides, and the command it was handling stays in the
//! mailbox for the next incarnation. A handler that returns an error is not
//! one: the cell keeps running in the same incarnation.

use std::time::Duration;

use claims::assert_ok;
use sorg_client::Client;
use sorg_common::{CellConfig, CellDeployment, DeployRequest, RestartPolicy, RestartType};
use sorg_tests::{TestApp, register_fixture_class, swarm_config};

use super::restart::{RESTART_WAIT, eventually, placement_gen};
use crate::integration::{spawn_test_app_with_swarm, to_sri};

const CRASH_LOGIC: &str = "../../tests/fixtures/cell-crash-logic";
const CRASH_CLASS: &str = "crash";
const CRASH_SRI: &str = "crash_cell";
// `Kv::new("crash")` stores `"crash/<key>"`.
const INCARNATION_KEY: &str = "crash/incarnation";
const HANDLED_KEY: &str = "crash/handled";

/// How long a handled command takes to show in the db.
const HANDLE_WAIT: Duration = Duration::from_secs(10);

async fn setup() -> (TestApp, Client) {
    let swarm = swarm_config!("cells/cells.jsonnet");
    register_fixture_class(CRASH_LOGIC, CRASH_CLASS, &swarm).await;
    let test_app = spawn_test_app_with_swarm(swarm).await;
    let client = Client::new(test_app.session().clone());
    (test_app, client)
}

/// Deploys the crash cell as a root under `restart_type`, 200 ms between
/// restarts.
async fn deploy_root(client: &Client, restart_type: RestartType) {
    let class = client
        .resolve_class_name("crash.wasm")
        .await
        .expect("failed to resolve the crash class")
        .expect("crash class is registered");
    let restart = RestartPolicy {
        restart_type,
        delay_ms: 200,
        ..RestartPolicy::default()
    };
    let deployment =
        CellDeployment::new(to_sri(CRASH_SRI), CellConfig::Wasm { class }).with_restart(restart);
    client
        .deploy_cells(DeployRequest::new(vec![deployment]))
        .await
        .expect("failed to deploy the crash root");
}

/// An `i32` the crash cell stored under `key`, once it has.
async fn stored(app: &TestApp, key: &str) -> Option<i32> {
    let raw = app.read_cell_kv(CRASH_SRI, key).await?;
    Some(assert_ok!(
        postcard::from_bytes(&raw),
        "decode the stored value"
    ))
}

async fn reaches(app: &TestApp, key: &str, want: i32, timeout: Duration) -> bool {
    eventually(
        timeout,
        || async move { stored(app, key).await == Some(want) },
    )
    .await
}

// A command, event and timer handler that return an error leave the cell
// running: a later command is handled by the same incarnation and the root is
// not restarted, although its policy would restart it after a crash.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn handler_crash_returned_errors_keep_the_cell_running() {
    // Arrange - the crash cell as a root that a crash would restart
    let (test_app, client) = setup().await;
    let sri = to_sri(CRASH_SRI);
    deploy_root(&client, RestartType::OnError).await;
    assert!(
        reaches(&test_app, INCARNATION_KEY, 1, HANDLE_WAIT).await,
        "crash cell did not initialise"
    );
    let first_gen = placement_gen(test_app.session(), sri)
        .await
        .expect("crash root is placed");

    // Act - a failing command, event and timer tick, each followed by a
    // succeeding marker that counts; one handler runs at a time.
    test_app.command_send(CRASH_SRI, "fail", None).await;
    test_app.command_send(CRASH_SRI, "bump", None).await;
    test_app.publish_cell_event("fail_evt", Vec::new()).await;
    test_app.publish_cell_event("mark_evt", Vec::new()).await;
    test_app
        .command_send(CRASH_SRI, "arm_fail_timer", None)
        .await;

    // Assert - the three markers ran in the first incarnation, which kept running
    assert!(
        reaches(&test_app, HANDLED_KEY, 3, HANDLE_WAIT).await,
        "the cell stopped handling messages after a returned error"
    );
    assert_eq!(
        stored(&test_app, INCARNATION_KEY).await,
        Some(1),
        "a returned error must not end the incarnation"
    );
    assert_eq!(
        placement_gen(test_app.session(), sri).await,
        Some(first_gen),
        "a returned error must not replace the instance"
    );
}
