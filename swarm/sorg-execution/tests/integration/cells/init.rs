//! Integration tests for cell state initialization at deploy time.
//!
//! Migrated to the new model: state lives in the DB (seeded once by `#[init]`),
//! commands are fire-and-forget, and the room cell publishes its temperature on
//! the `temperature` event for the host to observe.

use std::time::Duration;

use cell_protocol::Sri;
use claims::{assert_err, assert_ok};
use module_examples_common::Temperature;
use sorg_client::Client;
use sorg_common::{CellFailureKind, DeploymentError, RequirementTags};
use sorg_tests::{build_and_register_cell_class, swarm_config};

use crate::integration::spawn_test_app_with_swarm;

const ROOM_SRI: &str = "room_init_test";
const ROOM_CLASS: &str = "room.wasm";

const FAILING_INIT_SRI: &str = "failing_init_test";
const FAILING_INIT_CLASS: &str = "failing_init.wasm";

const NO_INIT_SRI: &str = "no_init_test";
const NO_INIT_CLASS: &str = "no_init.wasm";

/// When no instance state is pre-stored, deploying a cell with `#[init]` should
/// run init and produce the default state (temperature=20).
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn deploy_without_stored_state_runs_init() {
    // Arrange — build room cell (registers class), start swarm, no create_instance
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class("../../tests/fixtures/cell-room-logic", "room", &swarm).await;
    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut temp_q = test_app.subscribe_cell_event("temperature").await;

    // Act — deploy the cell directly (no pre-stored state)
    test_app.deploy_wasm_cell(ROOM_CLASS, ROOM_SRI).await;
    test_app
        .command_send(ROOM_SRI, "get_temperature", None)
        .await;

    // Assert — state should be the #[init] default (temperature=20)
    let received = assert_ok!(temp_q.receive().await);
    let temp = Temperature::from_payload(&received).expect("deser temperature");
    assert_eq!(
        temp.degrees_celsius, 20,
        "cell should use init default (20) when no stored state exists"
    );
}

/// Deploying with init arguments should seed the starting state from the
/// payload (the root/CLI counterpart to a spawner's `spawn_with`), instead of
/// the `#[init]` default.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn deploy_with_init_arguments_seeds_state() {
    // Arrange — build room cell (registers class), start swarm
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class("../../tests/fixtures/cell-room-logic", "room", &swarm).await;
    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut temp_q = test_app.subscribe_cell_event("temperature").await;
    let client = Client::new(test_app.session().clone());

    // Act — deploy with an init payload seeding a non-default temperature (42)
    let args = Temperature::new(42).to_payload().unwrap();
    assert_ok!(
        client
            .deploy_wasm_cell_with_arguments(
                Sri::from_target(ROOM_SRI).unwrap(),
                ROOM_CLASS,
                RequirementTags::default(),
                Some(args),
                None,
            )
            .await,
        "deploy with init arguments should succeed"
    );
    test_app
        .command_send(ROOM_SRI, "get_temperature", None)
        .await;

    // Assert — state seeded from the init args (42), not the init default (20)
    let received = assert_ok!(temp_q.receive().await);
    let temp = Temperature::from_payload(&received).expect("deser temperature");
    assert_eq!(
        temp.degrees_celsius, 42,
        "cell should seed state from init arguments, not the init default (20)"
    );
}

/// When `#[init]` returns `Err(...)` and no stored state exists, deploy should
/// fail with a `DeploymentFailed` error carrying the init error message.
/// No registry entry should be left behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn deploy_without_stored_state_init_fails() {
    // Arrange — build failing-init cell (registers class), start swarm
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class(
        "../../tests/fixtures/cell-failing-init-logic",
        "failing_init",
        &swarm,
    )
    .await;
    let test_app = spawn_test_app_with_swarm(swarm).await;
    let client = Client::new(test_app.session().clone());

    // Act — deploy the cell (no pre-stored state, init will fail)
    let result = test_app
        .try_deploy_wasm_cell(FAILING_INIT_CLASS, FAILING_INIT_SRI)
        .await;

    // Assert — deploy fails with RuntimeReported carrying the init error
    let err = assert_err!(result, "deploy should fail when init returns Err");
    let DeploymentError::DeploymentFailed(failures) = &err else {
        panic!("expected DeploymentFailed, got: {err:?}");
    };
    assert_eq!(1, failures.len(), "exactly one cell failure expected");
    let failure = &failures[0];
    assert!(
        matches!(&failure.kind, CellFailureKind::RuntimeReported(msg) if msg.contains("init")),
        "failure should be RuntimeReported with an init-related message, got: {:?}",
        failure.kind
    );

    // Assert — no orphan registry entry left behind
    assert_err!(
        client
            .inspect_instance(&Sri::from_target(FAILING_INIT_SRI).unwrap())
            .await,
        "no instance should be registered after failed init"
    );
}

/// Redeploying a cell should skip re-seeding and preserve the saved state.
/// Deploy → mutate state → undeploy → redeploy → state is preserved.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn redeploy_preserves_state_and_skips_init() {
    // Arrange — build room cell, start swarm
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class("../../tests/fixtures/cell-room-logic", "room", &swarm).await;
    let mut test_app = spawn_test_app_with_swarm(swarm).await;

    // First deploy — init runs, temp=20
    test_app.deploy_wasm_cell(ROOM_CLASS, ROOM_SRI).await;

    // Mutate state to temp=99 (fire-and-forget; allow it to be processed)
    let payload = Temperature::new(99).to_payload().unwrap();
    test_app
        .command_send(ROOM_SRI, "set_temperature", Some(payload))
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Undeploy
    test_app.undeploy_cell(ROOM_SRI).await;

    // Act — redeploy same SRI
    test_app.deploy_wasm_cell(ROOM_CLASS, ROOM_SRI).await;

    // Assert — state should be preserved (99), not re-seeded (20)
    let mut temp_q = test_app.subscribe_cell_event("temperature").await;
    test_app
        .command_send(ROOM_SRI, "get_temperature", None)
        .await;
    let received = assert_ok!(temp_q.receive().await);
    let temp = Temperature::from_payload(&received).expect("deser temperature");
    assert_eq!(
        temp.degrees_celsius, 99,
        "redeployed cell should use saved state (99), not re-seed init default (20)"
    );
}

/// Redeploying after a failed init should retry init, not permanently disable it.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn redeploy_after_failed_init_retries_init() {
    // Arrange — build failing-init cell, start swarm
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class(
        "../../tests/fixtures/cell-failing-init-logic",
        "failing_init",
        &swarm,
    )
    .await;
    let test_app = spawn_test_app_with_swarm(swarm).await;
    let client = Client::new(test_app.session().clone());

    // Act — first deploy fails
    let result = test_app
        .try_deploy_wasm_cell(FAILING_INIT_CLASS, FAILING_INIT_SRI)
        .await;
    assert_err!(&result, "first deploy should fail when init returns Err");

    // Assert — no orphan registry entry
    assert_err!(
        client
            .inspect_instance(&Sri::from_target(FAILING_INIT_SRI).unwrap())
            .await,
        "no instance should be registered after first failed init"
    );

    // Act — second deploy should also fail (init retries, not permanently disabled)
    let result = test_app
        .try_deploy_wasm_cell(FAILING_INIT_CLASS, FAILING_INIT_SRI)
        .await;

    // Assert — same init failure, proving init was retried
    let err = assert_err!(
        result,
        "second deploy should also fail when init returns Err"
    );
    let DeploymentError::DeploymentFailed(failures) = &err else {
        panic!("expected DeploymentFailed on redeploy, got: {err:?}");
    };
    assert_eq!(1, failures.len(), "exactly one cell failure expected");
    assert!(
        matches!(&failures[0].kind, CellFailureKind::RuntimeReported(msg) if msg.contains("init")),
        "failure should be RuntimeReported with init-related message, got: {:?}",
        failures[0].kind
    );

    // Assert — still no registry entry after second failure
    assert_err!(
        client
            .inspect_instance(&Sri::from_target(FAILING_INIT_SRI).unwrap())
            .await,
        "no instance should be registered after second failed init"
    );
}

/// When a cell has no `#[init]` and no stored state exists, it should deploy
/// and still be able to receive commands.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn deploy_without_stored_state_no_init() {
    // Arrange — build no-init cell (registers class), start swarm
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class("../../tests/fixtures/cell-no-init-logic", "no_init", &swarm)
        .await;
    let test_app = spawn_test_app_with_swarm(swarm).await;

    // Act — deploy the cell (no pre-stored state, no init)
    test_app.deploy_wasm_cell(NO_INIT_CLASS, NO_INIT_SRI).await;

    // Assert — cell is alive and can handle commands (fire-and-forget dispatch)
    test_app.command_send(NO_INIT_SRI, "ping", None).await;

    // Assert — the instance is registered
    let client = Client::new(test_app.session().clone());
    assert_ok!(
        client
            .inspect_instance(&Sri::from_target(NO_INIT_SRI).unwrap())
            .await,
        "inspect_instance should succeed for a deployed cell with no init"
    );
}
