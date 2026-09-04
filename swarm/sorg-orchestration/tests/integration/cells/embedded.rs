//! Integration tests for deploying cells to an *embedded* execution runtime.
//!
//! These exercise the orchestrator's handoff to an embedded runtime over the
//! DB-mailbox protocol, driven by [`MockEmbeddedExec`] in place of real
//! hardware. The orchestrator places an `embedded`-tagged cell on the embedded
//! runtime, writes a `DeploymentCommand` that points at the cell's artifact
//! root for the runtime's target, awaits the `DeploymentConfirmation`, and
//! registers the cell on success.
//!
//! Assertions are on orchestrator-observable behavior only: the placements,
//! the `DeploymentError` variant, and the command the mock consumed. The mock
//! never reads the artifact blobs, and whether the orchestrator made them
//! reachable is its own concern — not asserted here.

use std::str::FromStr;

use cell_protocol::{ArtifactPlatform, DeploymentCommand};
use claims::{assert_err, assert_matches, assert_none, assert_ok};
use sorg_common::{
    CellConfig, CellDeployment, CellFailureKind, DeployRequest, DeploymentError, RejectionReason,
    RequirementTags,
};
use sorg_tests::{
    DeployResponseMode, MockEmbeddedExec, Platform, build_and_register_cell_class, swarm_config,
};
use zenoh::config::ZenohId;

const FAILING_CLASS: &str = "failing_cell";

use crate::integration::{spawn_test_app_with_swarm, to_sri};

const APP_NAME: &str = "embedded_app";
const CELL_SRI: &str = "embedded-cell";
const CELL_CLASS: &str = "embedded_cell";

/// The artifact target of the embedded runtime under test. The mock registers
/// with [`Platform::Esp32c6`]'s tags; the orchestrator reads the matching
/// [`ArtifactPlatform`] back from those tags when routing the deploy dispatch.
const TARGET: ArtifactPlatform = ArtifactPlatform::Riscv32imac;

fn wasm_cell(sri: &str, class: &str) -> CellDeployment {
    CellDeployment::new(
        to_sri(sri),
        CellConfig::Wasm {
            class: class.to_owned(),
        },
    )
}

fn embedded_cell(sri: &str) -> CellDeployment {
    wasm_cell(sri, CELL_CLASS).with_tags(RequirementTags::new(vec![TARGET.as_str()]))
}

fn deploy_request(cells: Vec<CellDeployment>) -> DeployRequest {
    DeployRequest::new(
        cells
            .into_iter()
            .map(|cell| cell.with_app(Some(APP_NAME.to_owned())))
            .collect(),
    )
}

// Embedded mock present, esp32c6 AOT staged; deploy an embedded-tagged
// cell → placed on the mock, a DeploymentCommand carrying the class name is
// delivered, success is confirmed, and the cell is registered on the mock.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn deploy_success() {
    // Arrange — orch+db and a mock embedded runtime that confirms deployments
    let swarm = swarm_config!("cells/embedded/swarm.jsonnet");
    let test_app = spawn_test_app_with_swarm(swarm).await;
    let mock = MockEmbeddedExec::spawn(Platform::Esp32c6, DeployResponseMode::ConfirmSuccess).await;
    test_app
        .register_raw_aot(CELL_CLASS, TARGET, vec![0xA0], vec![0x4E])
        .await;
    let sorg = sorg_client::Client::new(test_app.session().clone());

    // Act — deploy an embedded-tagged cell
    assert_ok!(
        sorg.deploy_cells(deploy_request(vec![embedded_cell(CELL_SRI)]))
            .await,
        "embedded deploy should succeed when the runtime confirms"
    );

    // Assert — registered as a Wasm cell on the mock runtime
    let entry = assert_ok!(sorg.get_placement(&to_sri(CELL_SRI)).await)
        .expect("cell should be registered after deploy");
    assert_eq!(
        super::assert_wasm_runtime_id(&entry),
        mock.id(),
        "embedded cell should be registered on the mock embedded runtime"
    );

    // Assert — the orchestrator handed the mock exactly the command it needs:
    // the cell's SRI and the class name (firmware derives artifact paths from it).
    let received = mock.received_commands();
    assert_eq!(
        received.len(),
        1,
        "mock should have consumed exactly one deployment command, got {received:?}"
    );
    let DeploymentCommand::Deploy {
        sri: cmd_sri,
        class: cmd_class,
        ..
    } = &received[0]
    else {
        panic!("expected a Deploy command, got {:?}", received[0]);
    };
    assert_eq!(
        *cmd_sri,
        to_sri(CELL_SRI),
        "deployment command should name the deployed cell"
    );
    assert_eq!(
        cmd_class, CELL_CLASS,
        "deployment command should carry the cell's class name"
    );
}

// The runtime reports a failure (mock ConfirmFailure) → deploy fails
// with the message surfaced, the cell is not registered.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn deploy_runtime_reports_failure() {
    // Arrange — a mock embedded runtime that rejects the deployment
    let failure_msg = "embedded deploy rejected: out of flash";
    let swarm = swarm_config!("cells/embedded/swarm.jsonnet");
    let test_app = spawn_test_app_with_swarm(swarm).await;
    let mock = MockEmbeddedExec::spawn(
        Platform::Esp32c6,
        DeployResponseMode::ConfirmFailure(failure_msg.to_owned()),
    )
    .await;
    test_app
        .register_raw_aot(CELL_CLASS, TARGET, vec![0xA0], vec![0x4E])
        .await;
    let sorg = sorg_client::Client::new(test_app.session().clone());

    // Act — deploy an embedded-tagged cell the runtime will reject
    let err = assert_err!(
        sorg.deploy_cells(deploy_request(vec![embedded_cell(CELL_SRI)]))
            .await,
        "deploy should fail when the runtime reports a failure"
    );

    // Assert — DeploymentFailed naming the cell, the runtime, and the message
    let DeploymentError::DeploymentFailed(failures) = &err else {
        panic!("expected DeploymentFailed, got: {err:?}");
    };
    let failure = failures
        .iter()
        .find(|f| f.cell == to_sri(CELL_SRI))
        .unwrap_or_else(|| panic!("failure should name cell '{CELL_SRI}', got: {failures:?}"));
    assert_eq!(
        failure.runtime,
        mock.id(),
        "failure should name the embedded runtime the cell was placed on"
    );
    let CellFailureKind::RuntimeReported(msg) = &failure.kind else {
        panic!(
            "expected a RuntimeReported failure, got: {:?}",
            failure.kind
        );
    };
    assert!(
        msg.contains(failure_msg),
        "runtime-reported failure should carry the runtime's message, got: {msg}"
    );

    // Assert — the command was delivered to the mailbox, but the cell is not registered
    assert!(
        mock.received_commands().iter().any(
            |c| matches!(c, DeploymentCommand::Deploy { sri, .. } if *sri == to_sri(CELL_SRI))
        ),
        "mock should have received the deployment command before reporting failure"
    );
    assert!(
        assert_ok!(sorg.get_placement(&to_sri(CELL_SRI)).await).is_none(),
        "cell should not be registered after a failed deploy"
    );
}

// The runtime stays silent (mock Silent) → deploy fails with a timeout,
// the cell is not registered. The orchestrator bounds the confirmation-wait on
// the DB mailbox with a deadline.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn deploy_runtime_silent_times_out() {
    // Arrange — a mock embedded runtime that consumes commands but never confirms
    let swarm = swarm_config!("cells/embedded/swarm.jsonnet");
    let test_app = spawn_test_app_with_swarm(swarm).await;
    let mock = MockEmbeddedExec::spawn(Platform::Esp32c6, DeployResponseMode::Silent).await;
    test_app
        .register_raw_aot(CELL_CLASS, TARGET, vec![0xA0], vec![0x4E])
        .await;
    let sorg = sorg_client::Client::new(test_app.session().clone());

    // Act — deploy an embedded-tagged cell the runtime will never confirm
    let err = assert_err!(
        sorg.deploy_cells(deploy_request(vec![embedded_cell(CELL_SRI)]))
            .await,
        "deploy should fail when the runtime never confirms"
    );

    // Assert — DeploymentFailed with a Timeout for the cell, on the mock runtime
    let DeploymentError::DeploymentFailed(failures) = &err else {
        panic!("expected DeploymentFailed, got: {err:?}");
    };
    let failure = failures
        .iter()
        .find(|f| f.cell == to_sri(CELL_SRI))
        .unwrap_or_else(|| panic!("failure should name cell '{CELL_SRI}', got: {failures:?}"));
    assert_eq!(
        failure.runtime,
        mock.id(),
        "timeout failure should name the embedded runtime the cell was placed on"
    );
    assert!(
        matches!(failure.kind, CellFailureKind::Timeout),
        "expected a Timeout failure, got: {:?}",
        failure.kind
    );

    // Assert — the command was delivered (the runtime chose silence, the deploy
    // did not merely fail to reach it), but the cell is not registered
    assert!(
        mock.received_commands().iter().any(
            |c| matches!(c, DeploymentCommand::Deploy { sri, .. } if *sri == to_sri(CELL_SRI))
        ),
        "mock should have consumed the command before staying silent"
    );
    assert!(
        assert_ok!(sorg.get_placement(&to_sri(CELL_SRI)).await).is_none(),
        "cell should not be registered after a timed-out deploy"
    );
}

// A linux exec and an embedded mock are both present and the class has
// both artifacts staged; an embedded-tagged cell lands on the mock, never the
// linux exec — routing is decided by the tag, not artifact availability.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn embedded_tag_routes_to_mock_not_linux() {
    // Arrange — orch+db, a linux exec distractor, and the embedded mock
    let swarm = swarm_config!("cells/embedded/swarm.jsonnet");
    let _exec_linux = swarm_config!("cells/embedded/exec_linux.jsonnet");
    let test_app = spawn_test_app_with_swarm(swarm).await;
    let mock = MockEmbeddedExec::spawn(Platform::Esp32c6, DeployResponseMode::ConfirmSuccess).await;

    // Both artifacts available, so only the `embedded` tag can decide placement.
    test_app
        .register_raw_class(CELL_CLASS, vec![0x00, 0x61, 0x73, 0x6D])
        .await;
    test_app
        .register_raw_aot(CELL_CLASS, TARGET, vec![0xA0], vec![0x4E])
        .await;
    let sorg = sorg_client::Client::new(test_app.session().clone());

    // Act — deploy an embedded-tagged cell
    assert_ok!(
        sorg.deploy_cells(deploy_request(vec![embedded_cell(CELL_SRI)]))
            .await,
        "embedded deploy should succeed when the runtime confirms"
    );

    // Assert — landed on the embedded mock, not the linux exec
    let entry = assert_ok!(sorg.get_placement(&to_sri(CELL_SRI)).await)
        .expect("cell should be registered after deploy");
    assert_eq!(
        super::assert_wasm_runtime_id(&entry),
        mock.id(),
        "embedded-tagged cell should land on the embedded mock, not the linux exec"
    );
}

// An untagged cell whose class has a wasm binary but no AOT, with both a
// linux exec and an embedded mock present, lands on the linux exec — the
// embedded runtime is ruled out by the missing AOT, narrowing placement to the
// only runtime that can load the available artifact.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn artifact_narrows_untagged_cell_to_linux() {
    // Arrange — orch+db, a linux exec and an embedded mock; only the wasm staged.
    // A real wasm is built because the cell actually loads on the linux exec.
    let swarm = swarm_config!("cells/embedded/swarm.jsonnet");
    let _exec_linux = swarm_config!("cells/embedded/exec_linux.jsonnet");
    build_and_register_cell_class(
        "../../tests/fixtures/dummy_cell",
        "embedded_routing",
        &swarm,
    )
    .await;
    let test_app = spawn_test_app_with_swarm(swarm).await;
    let mock = MockEmbeddedExec::spawn(Platform::Esp32c6, DeployResponseMode::ConfirmSuccess).await;
    let sorg = sorg_client::Client::new(test_app.session().clone());

    // Act — deploy an untagged cell whose class only has a wasm artifact
    assert_ok!(
        sorg.deploy_cells(deploy_request(vec![wasm_cell(
            CELL_SRI,
            "embedded_routing.wasm"
        )]))
        .await,
        "deploy should succeed on the linux exec when only the wasm artifact is available"
    );

    // Assert — landed on the linux exec, not the embedded mock
    let entry = assert_ok!(sorg.get_placement(&to_sri(CELL_SRI)).await)
        .expect("cell should be registered after deploy");
    assert_ne!(
        super::assert_wasm_runtime_id(&entry),
        mock.id(),
        "untagged cell with only a wasm artifact should land on the linux exec, not the mock"
    );
}

// An embedded runtime hosts a single cell. With one mock present, a
// first embedded cell deploys successfully; a second embedded cell is then
// infeasible — the only embedded runtime is at capacity — leaving the first cell
// untouched and the second unregistered.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn embedded_at_capacity_is_infeasible() {
    // Arrange — orch+db and a single embedded mock
    let swarm = swarm_config!("cells/embedded/swarm.jsonnet");
    let test_app = spawn_test_app_with_swarm(swarm).await;
    let mock = MockEmbeddedExec::spawn(Platform::Esp32c6, DeployResponseMode::ConfirmSuccess).await;
    test_app
        .register_raw_aot(CELL_CLASS, TARGET, vec![0xA0], vec![0x4E])
        .await;
    let sorg = sorg_client::Client::new(test_app.session().clone());

    // Act 1 — a first embedded cell occupies the mock
    assert_ok!(
        sorg.deploy_cells(DeployRequest::new(vec![
            embedded_cell("embedded-cell-first").with_app(Some("embedded_app_first".to_owned())),
        ]))
        .await,
        "first embedded cell should deploy onto the mock"
    );

    // Act 2 — a second embedded cell has no free embedded runtime
    let err = assert_err!(
        sorg.deploy_cells(DeployRequest::new(vec![
            embedded_cell("embedded-cell-second").with_app(Some("embedded_app_second".to_owned())),
        ]))
        .await,
        "second embedded cell should fail when the only embedded runtime is occupied"
    );

    // Assert — infeasible because the mock is at capacity
    let DeploymentError::Infeasible(cells) = &err else {
        panic!("expected Infeasible, got: {err:?}");
    };
    let cell = cells
        .iter()
        .find(|c| c.cell == to_sri("embedded-cell-second"))
        .unwrap_or_else(|| panic!("infeasibility should name the second cell, got: {cells:?}"));
    assert!(
        cell.rejections
            .iter()
            .any(|r| r.runtime == mock.id() && matches!(r.reason, RejectionReason::AtCapacity)),
        "expected the mock rejected as AtCapacity, got: {:?}",
        cell.rejections
    );

    // Assert — the first cell is untouched, the second is not registered
    let first = assert_ok!(sorg.get_placement(&to_sri("embedded-cell-first")).await)
        .expect("first cell should remain registered");
    assert_eq!(
        super::assert_wasm_runtime_id(&first),
        mock.id(),
        "first cell should still be on the mock after the second deploy fails"
    );
    assert!(
        assert_ok!(sorg.get_placement(&to_sri("embedded-cell-second")).await).is_none(),
        "second cell should not be registered after an infeasible deploy"
    );
}

// Two embedded mocks are present; an app with two embedded cells places
// one cell on each — embedded cells do not consolidate onto a single runtime
// (each embedded runtime hosts a single cell), unlike untagged cells.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn embedded_cells_do_not_consolidate() {
    // Arrange — orch+db and two embedded mocks
    let swarm = swarm_config!("cells/embedded/swarm.jsonnet");
    let test_app = spawn_test_app_with_swarm(swarm).await;
    let mock_a =
        MockEmbeddedExec::spawn(Platform::Esp32c6, DeployResponseMode::ConfirmSuccess).await;
    let mock_b =
        MockEmbeddedExec::spawn(Platform::Esp32c6, DeployResponseMode::ConfirmSuccess).await;
    test_app
        .register_raw_aot(CELL_CLASS, TARGET, vec![0xA0], vec![0x4E])
        .await;
    let sorg = sorg_client::Client::new(test_app.session().clone());

    // Act — deploy an app with two embedded cells
    assert_ok!(
        sorg.deploy_cells(deploy_request(vec![
            embedded_cell("embedded-cell-a"),
            embedded_cell("embedded-cell-b"),
        ]))
        .await,
        "an app with two embedded cells should deploy across the two mocks"
    );

    // Assert — the two cells landed on distinct runtimes, one per mock
    let node_a = super::assert_wasm_runtime_id(
        &assert_ok!(sorg.get_placement(&to_sri("embedded-cell-a")).await)
            .expect("cell a should be registered"),
    );
    let node_b = super::assert_wasm_runtime_id(
        &assert_ok!(sorg.get_placement(&to_sri("embedded-cell-b")).await)
            .expect("cell b should be registered"),
    );

    assert_ne!(
        node_a, node_b,
        "embedded cells should not consolidate onto a single runtime"
    );
    let mocks = [mock_a.id(), mock_b.id()];
    assert!(
        mocks.contains(&node_a) && mocks.contains(&node_b),
        "both cells should land on the embedded mocks, got {node_a} and {node_b}"
    );
}

// Delete an app whose cell lives on an embedded runtime. The
// orchestrator should send a DeploymentCommand::Delete to the mock's mailbox
// (not a zenoh query), and the cell should be deregistered afterward.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn delete_embedded_cell() {
    // Arrange — deploy an embedded cell successfully
    let swarm = swarm_config!("cells/embedded/swarm.jsonnet");
    let test_app = spawn_test_app_with_swarm(swarm).await;
    let mock = MockEmbeddedExec::spawn(Platform::Esp32c6, DeployResponseMode::ConfirmSuccess).await;
    test_app
        .register_raw_aot(CELL_CLASS, TARGET, vec![0xA0], vec![0x4E])
        .await;
    let sorg = sorg_client::Client::new(test_app.session().clone());
    assert_ok!(
        sorg.deploy_cells(deploy_request(vec![embedded_cell(CELL_SRI)]))
            .await,
        "setup: embedded deploy should succeed"
    );

    // Act — delete the application
    assert_ok!(
        sorg.delete_application(APP_NAME).await,
        "deleting an app with an embedded cell should succeed"
    );

    // Assert — cell is deregistered and the mock received a Delete command
    assert!(
        assert_ok!(sorg.get_placement(&to_sri(CELL_SRI)).await).is_none(),
        "cell should be deregistered after deleting the application"
    );
    let expected_sri = to_sri(CELL_SRI);
    assert!(
        mock.received_commands()
            .iter()
            .any(|c| matches!(c, DeploymentCommand::Delete { sri } if *sri == expected_sri)),
        "mock should have received a Delete command for '{CELL_SRI}' during application deletion"
    );
}

// The runtime goes offline after a successful deploy and never confirms the
// Delete command. Releasing the registry rows is the authoritative delete —
// the exec teardown is only the fast-path kill — so the delete still
// succeeds and the cell is deregistered; a silent runtime must not leave a
// corpse that blocks the SRI from redeployment.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn delete_runtime_silent_releases_rows() {
    // Arrange — deploy an embedded cell successfully
    let swarm = swarm_config!("cells/embedded/swarm.jsonnet");
    let test_app = spawn_test_app_with_swarm(swarm).await;
    let mock = MockEmbeddedExec::spawn(Platform::Esp32c6, DeployResponseMode::ConfirmSuccess).await;
    test_app
        .register_raw_aot(CELL_CLASS, TARGET, vec![0xA0], vec![0x4E])
        .await;
    let sorg = sorg_client::Client::new(test_app.session().clone());
    assert_ok!(
        sorg.deploy_cells(deploy_request(vec![embedded_cell(CELL_SRI)]))
            .await,
        "setup: embedded deploy should succeed"
    );

    // Stop the mock — its poll task aborts, so the Delete command will be
    // written to the mailbox but never consumed or confirmed.
    mock.kill();

    // Act — delete the application; the runtime will never confirm deletion
    assert_ok!(
        sorg.delete_application(APP_NAME).await,
        "delete should succeed even when the runtime never confirms"
    );

    // Assert — the cell is deregistered despite the silent runtime
    assert_none!(
        assert_ok!(sorg.get_placement(&to_sri(CELL_SRI)).await),
        "cell should be deregistered after deleting the application"
    );
}

// When a multi-cell app deploy fails (one cell's runtime reports a
// failure), the orchestrator rolls back the successfully-deployed cells. For an
// embedded cell that was deployed successfully, the rollback should send a
// DeploymentCommand::Delete to its runtime's mailbox.
//
// Setup: an embedded mock (ConfirmSuccess) + a linux exec. The embedded cell
// (class with AOT) goes to the mock and succeeds; a linux cell (class with
// garbage wasm, no AOT) goes to the linux exec and fails at load time. The
// rollback should tear down the embedded cell via the mailbox.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn rollback_tears_down_embedded_cell() {
    // Arrange — an embedded mock and a linux exec; two different classes
    let swarm = swarm_config!("cells/embedded/swarm.jsonnet");
    let _exec_linux = swarm_config!("cells/embedded/exec_linux.jsonnet");
    let test_app = spawn_test_app_with_swarm(swarm).await;
    let mock = MockEmbeddedExec::spawn(Platform::Esp32c6, DeployResponseMode::ConfirmSuccess).await;
    test_app
        .register_raw_aot(CELL_CLASS, TARGET, vec![0xA0], vec![0x4E])
        .await;
    test_app
        .register_raw_class(FAILING_CLASS, vec![0xFF, 0xFF])
        .await;
    let sorg = sorg_client::Client::new(test_app.session().clone());

    // Act — deploy a two-cell app: one embedded (succeeds) + one linux (fails)
    let err = assert_err!(
        sorg.deploy_cells(deploy_request(vec![
            embedded_cell("cell-ok"),
            wasm_cell("cell-fail", FAILING_CLASS),
        ]))
        .await,
        "deploy should fail when the linux cell fails to load"
    );

    // Assert — DeploymentFailed
    assert!(
        matches!(&err, DeploymentError::DeploymentFailed(_)),
        "expected DeploymentFailed, got: {err:?}"
    );

    // Assert — neither cell is registered (rollback cleaned up the successful one)
    assert!(
        assert_ok!(sorg.get_placement(&to_sri("cell-ok")).await).is_none(),
        "successfully-deployed embedded cell should be deregistered after rollback"
    );
    assert!(
        assert_ok!(sorg.get_placement(&to_sri("cell-fail")).await).is_none(),
        "failed cell should not be registered"
    );

    // Assert — the mock received a Delete command (the rollback teardown)
    let expected_sri = to_sri("cell-ok");
    assert!(
        mock.received_commands()
            .iter()
            .any(|c| matches!(c, DeploymentCommand::Delete { sri } if *sri == expected_sri)),
        "mock should have received a Delete command for 'cell-ok' during rollback of the embedded cell"
    );
}

// An embedded mock joins, registers in the exec registry, and then crashes
// (kill). The orchestrator's introspection plugin should detect the liveliness
// Delete and deregister the mock from the exec registry.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn embedded_leave_deregisters_exec() {
    // Arrange — orch+db and a mock embedded runtime with a short lease so
    // liveliness expiry is detected quickly after kill.
    let swarm = swarm_config!("cells/embedded/swarm.jsonnet");
    let test_app = spawn_test_app_with_swarm(swarm).await;

    let mock_zid = "aabbccdd00112233aabbccdd00112233";
    let mut config = zenoh::Config::default();
    config
        .set_id(Some(ZenohId::from_str(mock_zid).unwrap()))
        .unwrap();
    config
        .insert_json5("transport/link/tx/lease", "1000")
        .unwrap();
    config
        .insert_json5("transport/link/tx/keep_alive", "4")
        .unwrap();

    let mock = MockEmbeddedExec::spawn_with_config(
        Platform::Esp32c6,
        DeployResponseMode::ConfirmSuccess,
        config,
    )
    .await;

    // Assert — mock appears in the exec registry
    test_app.wait_for_registered_exec(mock_zid).await;

    // Act — kill the mock (drops the zenoh session → liveliness Delete)
    mock.kill();

    // Assert — mock is removed from the exec registry
    test_app.wait_for_deregistered_exec(mock_zid).await;
}

// Two embedded cells in one app request, but only one embedded runtime present.
// Both cells are trivially placed on the sole runtime (capacity = 1), which is
// a conflict the placement must detect and surface as PlacementConflicts rather
// than over-assigning the runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn embedded_trivial_conflict_is_infeasible() {
    // Arrange — orch+db and a single embedded mock (empty)
    let swarm = swarm_config!("cells/embedded/swarm.jsonnet");
    let test_app = spawn_test_app_with_swarm(swarm).await;
    let _mock =
        MockEmbeddedExec::spawn(Platform::Esp32c6, DeployResponseMode::ConfirmSuccess).await;
    test_app
        .register_raw_aot(CELL_CLASS, TARGET, vec![0xA0], vec![0x4E])
        .await;
    let sorg = sorg_client::Client::new(test_app.session().clone());

    // Act — deploy an app with two embedded cells competing for the single runtime
    let err = assert_err!(
        sorg.deploy_cells(deploy_request(vec![
            embedded_cell("embedded-cell-a"),
            embedded_cell("embedded-cell-b"),
        ]))
        .await,
        "deploying two embedded cells onto one runtime should fail"
    );

    // Assert — PlacementConflicts: the cells conflict over the single capacity-1 runtime
    assert_matches!(
        err,
        DeploymentError::PlacementConflicts,
        "expected PlacementConflicts"
    );

    // Assert — neither cell was registered
    assert_none!(
        assert_ok!(sorg.get_placement(&to_sri("embedded-cell-a")).await),
        "cell-a should not be registered after a placement conflict"
    );
    assert_none!(
        assert_ok!(sorg.get_placement(&to_sri("embedded-cell-b")).await),
        "cell-b should not be registered after a placement conflict"
    );
}
