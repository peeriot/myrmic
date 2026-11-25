//! A trap or panic in a handler is a crash: the incarnation ends, a root's
//! restart policy decides, and the command it was handling stays in the
//! mailbox for the next incarnation. A handler that returns an error is not
//! one: the cell keeps running in the same incarnation.

use std::time::Duration;

use claims::assert_ok;
use sorg_client::Client;
use sorg_common::{
    CellConfig, CellDeployment, DeployRequest, LostReason, RestartPolicy, RestartType, root_death,
    root_restart,
};
use sorg_tests::{TestApp, register_fixture_class, swarm_config};

use super::restart::{RESTART_WAIT, eventually, placement_gen};
use crate::integration::{spawn_test_app_with_swarm, to_sri};

const CRASH_LOGIC: &str = "../../tests/fixtures/cell-crash-logic";
const CRASH_CLASS: &str = "crash";
const CRASH_SRI: &str = "crash_cell";
// `Kv::new("crash")` stores `"crash/<key>"`.
const INCARNATION_KEY: &str = "crash/incarnation";
const HANDLED_KEY: &str = "crash/handled";
const PANIC_ONCE_KEY: &str = "crash/panic_once";
const CRASHED_WRITE_KEY: &str = "crash/crashed_write";

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

// A panic in a command handler of an `on-error` root is a crash: a `Crashed`
// root death is recorded, the root comes back as a new instance (init runs
// again), and the command that crashed it is delivered to that instance. The
// crashed attempt's write is rolled back.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn handler_crash_on_error_root_restarts_and_gets_the_command_again() {
    // Arrange
    let (test_app, client) = setup().await;
    let sri = to_sri(CRASH_SRI);
    deploy_root(&client, RestartType::OnError).await;
    assert!(
        reaches(&test_app, INCARNATION_KEY, 1, HANDLE_WAIT).await,
        "crash cell did not initialise"
    );
    let session = test_app.session();
    let first_gen = placement_gen(session, sri)
        .await
        .expect("crash root is placed");

    // Act - a command that panics while the incarnation counter is 1
    test_app.command_send(CRASH_SRI, "panic_once", None).await;

    // Assert I - the root died as a crash. The record lingers until the exec
    // released the placement and the leader acted on it, so polling sees it.
    assert!(
        eventually(RESTART_WAIT, || async move {
            root_death::list(session).await.is_ok_and(|deaths| {
                deaths
                    .iter()
                    .any(|death| death.sri == sri && death.reason == LostReason::Crashed)
            })
        })
        .await,
        "no Crashed root death was recorded for the panicking root"
    );

    // Assert II - restarted as a new instance whose init ran again
    assert!(
        eventually(RESTART_WAIT, || async move {
            placement_gen(session, sri)
                .await
                .is_some_and(|gen_id| gen_id > first_gen)
        })
        .await,
        "the crashed root was not re-placed at a newer generation"
    );
    assert!(
        reaches(&test_app, INCARNATION_KEY, 2, RESTART_WAIT).await,
        "the restarted root did not run init again"
    );

    // Assert III - the crashing command reached the new incarnation, and the
    // crashed attempt left nothing behind
    assert!(
        reaches(&test_app, PANIC_ONCE_KEY, 2, HANDLE_WAIT).await,
        "the command that crashed the root was not delivered to the new incarnation"
    );
    assert_eq!(
        stored(&test_app, CRASHED_WRITE_KEY).await,
        None,
        "a write from the crashed attempt is visible"
    );
}

// A `never` root that panics in a command handler stays down; deployed again
// under the same name, the new incarnation receives the command that crashed
// the first one.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn handler_crash_never_root_stays_down_and_a_redeploy_gets_the_command() {
    // Arrange
    let (test_app, client) = setup().await;
    deploy_root(&client, RestartType::Never).await;
    assert!(
        reaches(&test_app, INCARNATION_KEY, 1, HANDLE_WAIT).await,
        "crash cell did not initialise"
    );

    // Act I - a command that panics while the incarnation counter is 1
    test_app.command_send(CRASH_SRI, "panic_once", None).await;

    // Assert I - the root ended: the exec released its placement, there is no
    // spec to restart it from, and no second init ran
    let app = &test_app;
    assert!(
        eventually(RESTART_WAIT, || async move {
            !app.is_cell_registered(CRASH_SRI).await
        })
        .await,
        "the root kept running after a handler panic"
    );
    assert!(
        root_restart::list_specs(test_app.session())
            .await
            .expect("failed to list restart specs")
            .is_empty(),
        "a never root has no restart spec"
    );
    assert_eq!(
        stored(&test_app, INCARNATION_KEY).await,
        Some(1),
        "a never root must not be restarted"
    );

    // Act II - deploy it again under the same name
    test_app.deploy_wasm_cell("crash.wasm", CRASH_SRI).await;

    // Assert II - the new incarnation gets the command that crashed the first
    assert!(
        reaches(&test_app, INCARNATION_KEY, 2, HANDLE_WAIT).await,
        "the redeployed root did not initialise"
    );
    assert!(
        reaches(&test_app, PANIC_ONCE_KEY, 2, HANDLE_WAIT).await,
        "the command that crashed the root was not delivered to the redeployed incarnation"
    );
    assert_eq!(
        stored(&test_app, CRASHED_WRITE_KEY).await,
        None,
        "a write from the crashed attempt is visible"
    );
}

// A panic in an event handler is a crash. Under `on-error` only an abnormal
// death restarts a root, and on a live node with no spawn or stop that is
// `Crashed`, so a second init proves the reason. The event is not delivered
// again, so the restarted root stays up.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn handler_crash_event_handler_panic_crashes_the_root() {
    // Arrange
    let (test_app, client) = setup().await;
    deploy_root(&client, RestartType::OnError).await;
    assert!(
        reaches(&test_app, INCARNATION_KEY, 1, HANDLE_WAIT).await,
        "crash cell did not initialise"
    );

    // Act - an event whose handler panics
    test_app.publish_cell_event("crash_evt", Vec::new()).await;

    // Assert - the root crashed and was restarted
    assert!(
        reaches(&test_app, INCARNATION_KEY, 2, RESTART_WAIT).await,
        "an event-handler panic did not crash and restart the root"
    );
}

// A panic in a timer tick is a crash, observed the same way. Timers die with
// the incarnation, so the restarted root does not tick again.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn handler_crash_timer_handler_panic_crashes_the_root() {
    // Arrange
    let (test_app, client) = setup().await;
    deploy_root(&client, RestartType::OnError).await;
    assert!(
        reaches(&test_app, INCARNATION_KEY, 1, HANDLE_WAIT).await,
        "crash cell did not initialise"
    );

    // Act - arm a timer whose tick panics 100 ms later
    test_app
        .command_send(CRASH_SRI, "arm_panic_timer", None)
        .await;

    // Assert - the root crashed and was restarted
    assert!(
        reaches(&test_app, INCARNATION_KEY, 2, RESTART_WAIT).await,
        "a timer-handler panic did not crash and restart the root"
    );
}
