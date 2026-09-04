use std::time::Duration;

use claims::{assert_err, assert_matches};
use sorg_common::DeploymentError;
use sorg_tests::{TestApp, build_and_register_cell_class, swarm_config};

use crate::integration::{spawn_empty_test_app, spawn_test_app_with_swarm};

const DUMMY_CELL_SRI: &str = "dummy_cell";

// Liveness cell: `cell-counter-logic` (already on the new SDK). Its fire-and-forget
// `increment` command bumps an `i32` stored at `counter/count` in the cell's own KV space —
// the observable effect we assert on now that commands don't return a value.
const COUNTER_LOGIC: &str = "../../tests/fixtures/cell-counter-logic";
const COUNTER_CLASS: &str = "counter";
const COUNTER_SRI: &str = "counter_cell";
// The cell's `Kv::new("counter/")` prefix already ends in `/`, and `Kv::full_key` inserts
// another separator — so the actual stored key is `counter//count` (double slash). We read
// the exact key the guest writes.
const COUNT_KEY: &str = "counter//count";

/// Encodes an `increment` payload. The handler takes a bare `i32`, which the SDK
/// carries on the wire as a JSON number.
fn incr_payload(by: i32) -> Vec<u8> {
    serde_json::to_vec(&by).expect("encode increment payload")
}

/// Polls the counter cell's stored count until it equals `want`, returning whether it did
/// within `attempts` × 100ms. `increment` is fire-and-forget, so the DB write lands
/// asynchronously — we poll rather than read once.
async fn count_reaches(app: &TestApp, sri: &str, want: i32, attempts: u32) -> bool {
    for _ in 0..attempts {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Some(raw) = app.read_cell_kv(sri, COUNT_KEY).await
            && matches!(postcard::from_bytes::<i32>(&raw), Ok(n) if n == want)
        {
            return true;
        }
    }
    false
}

// A freshly deployed wasm cell is not merely registered but actually loaded and executing
// on an exec runtime: a fire-and-forget `increment` reaches the cell and its DB effect
// becomes observable.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn deploy_cell() {
    // Arrange - build + register the counter cell
    let swarm = swarm_config!("cells/cells.jsonnet");
    build_and_register_cell_class(COUNTER_LOGIC, COUNTER_CLASS, &swarm).await;

    let test_app = spawn_test_app_with_swarm(swarm).await;

    // Act - deploy the counter cell
    test_app.deploy_wasm_cell("counter.wasm", COUNTER_SRI).await;

    // Assert - registered, and actually running: increment it and observe the DB effect
    assert!(test_app.is_cell_registered(COUNTER_SRI).await);
    test_app
        .command_send(COUNTER_SRI, "increment", Some(incr_payload(1)))
        .await;
    assert!(
        count_reaches(&test_app, COUNTER_SRI, 1, 25).await,
        "deployed counter cell should process the increment (count -> 1)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn load_error_no_exec() {
    // Arrange - orch only, no exec runtime
    let swarm = swarm_config!("cells/orch_only.jsonnet");
    build_and_register_cell_class("../../tests/fixtures/dummy_cell", "dummy", &swarm).await;

    let test_app = spawn_test_app_with_swarm(swarm).await;

    // Act - try to load the dummy cell;
    let result = test_app
        .try_deploy_wasm_cell("dummy_cell.wasm", DUMMY_CELL_SRI)
        .await;

    // Assert - no exec runtime is registered at all
    assert_matches!(
        assert_err!(result),
        DeploymentError::NoRuntimesAvailable,
        "expected NoRuntimesAvailable when no exec runtime is registered"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn load_error_no_orch() {
    // Arrange - bare peer, no orch or exec
    let test_app = spawn_empty_test_app().await;

    // Act - try to load a cell when no orch is available
    let result = test_app
        .try_deploy_wasm_cell("dummy_cell.wasm", DUMMY_CELL_SRI)
        .await;

    // Assert - no orchestrator responded
    assert_matches!(
        assert_err!(result),
        DeploymentError::OrchestratorUnreachable,
        "expected OrchestratorUnreachable when no orchestrator is present"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn load_error_missing_binary() {
    // Arrange - orch + exec, but no binary uploaded
    let swarm = swarm_config!("cells/cells.jsonnet");
    let test_app = spawn_test_app_with_swarm(swarm).await;

    // Act - try to load a cell whose binary doesn't exist
    let result = test_app
        .try_deploy_wasm_cell("nonexistent.wasm", DUMMY_CELL_SRI)
        .await;

    // Assert - infeasible: the linux exec is rejected for the missing wasm artifact
    assert_matches!(
        assert_err!(result),
        DeploymentError::Infeasible(_),
        "expected Infeasible with a missing-artifact rejection"
    );
}

const MISSING_CELL_SRI: &str = "a1b2c3d4-5e6f-7g8h-9i0j-k1l2m3n4o5p6";

// A *failed* deploy of one cell must not disturb an already-running cell: the incumbent
// stays registered AND keeps executing (proven by a post-failure increment landing).
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn load_error_missing_binary_does_not_affect_existing_cell() {
    // Arrange - deploy the counter cell successfully
    let swarm = swarm_config!("cells/cells.jsonnet");
    build_and_register_cell_class(COUNTER_LOGIC, COUNTER_CLASS, &swarm).await;

    let test_app = spawn_test_app_with_swarm(swarm).await;
    test_app.deploy_wasm_cell("counter.wasm", COUNTER_SRI).await;

    // Act - try to load a cell with a missing binary
    let result = test_app
        .try_deploy_wasm_cell("nonexistent.wasm", MISSING_CELL_SRI)
        .await;

    // Assert I - the load should fail
    assert_err!(result);

    // Assert II - the counter cell is untouched and still executing
    assert!(test_app.is_cell_registered(COUNTER_SRI).await);
    test_app
        .command_send(COUNTER_SRI, "increment", Some(incr_payload(1)))
        .await;
    assert!(
        count_reaches(&test_app, COUNTER_SRI, 1, 25).await,
        "existing counter cell should still process commands after the failed deploy"
    );

    // Assert III - the missing cell should not be in the registry
    assert!(!test_app.is_cell_registered(MISSING_CELL_SRI).await);
}

// Undeploy both deregisters the cell AND tears down the running instance: after undeploy a
// further `increment` is not processed, so the count never advances past its pre-undeploy
// value.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn undeploy_cell() {
    // Arrange - deploy the counter cell
    let swarm = swarm_config!("cells/cells.jsonnet");
    build_and_register_cell_class(COUNTER_LOGIC, COUNTER_CLASS, &swarm).await;

    let test_app = spawn_test_app_with_swarm(swarm).await;
    test_app.deploy_wasm_cell("counter.wasm", COUNTER_SRI).await;

    // Assert I - registered and executing (count -> 1)
    assert!(test_app.is_cell_registered(COUNTER_SRI).await);
    test_app
        .command_send(COUNTER_SRI, "increment", Some(incr_payload(1)))
        .await;
    assert!(
        count_reaches(&test_app, COUNTER_SRI, 1, 25).await,
        "counter cell should process the increment before undeploy (count -> 1)"
    );

    // Act - delete the cell
    test_app.undeploy_cell(COUNTER_SRI).await;

    // Assert II - cell is no longer in the registry
    assert!(!test_app.is_cell_registered(COUNTER_SRI).await);

    // Assert III - the instance was torn down: a further increment is not processed, so the
    // count does not advance to 2.
    let err = assert_err!(
        test_app
            .try_command_send(COUNTER_SRI, "increment", Some(incr_payload(1)))
            .await,
        "commanding an undeployed bridge should be rejected"
    )
    .to_string();
    assert!(
        err.contains("has no placement"),
        "expected a 'has no placement' error, got: {err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn delete_error_no_orch() {
    // Arrange - bare peer, no orch or exec
    let test_app = spawn_empty_test_app().await;

    // Act - try to delete a cell when no orch is available
    let result = test_app.try_undeploy_cell(DUMMY_CELL_SRI).await;

    // Assert - should get an error mentioning the missing orch
    let err_msg = assert_err!(result).to_string();
    assert!(
        err_msg.contains("no response from an orchestration runtime"),
        "expected 'no response from an orchestration runtime' error, got: {err_msg}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn delete_error_cell_not_deployed() {
    // Arrange - orch + exec, but no cell deployed
    let swarm = swarm_config!("cells/cells.jsonnet");
    let test_app = spawn_test_app_with_swarm(swarm).await;

    // Act - try to delete a cell that was never deployed
    let result = test_app.try_undeploy_cell(DUMMY_CELL_SRI).await;

    // Assert - should get an error about the cell not being deployed
    let err_msg = assert_err!(result).to_string();
    assert!(
        err_msg.contains("not deployed"),
        "expected 'not deployed' error, got: {err_msg}"
    );
}
