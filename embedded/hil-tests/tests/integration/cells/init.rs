//! Tests on Init (manually mirrors tests in
//! `swarm/sorg-execution/tests/integration/cells/init.rs`)

use claims::{assert_err, assert_ok};
use module_examples_common::Temperature;
use sorg_common::{CellFailureKind, DeploymentError};
use test_framework::scenario::SwarmTestCtx;

use crate::integration::{
    aot::{aot_class_name, build_aot_cell},
    device_present,
    espflash::flash_device,
    hil_swarm_test,
};

const ROOM_CELL: &str = "cell-room-logic";
const FAILING_INIT_CELL: &str = "cell-failing-init-logic";
const NO_INIT_CELL: &str = "cell-no-init-logic";

const ROOM_SRI: &str = "room_init_test";
const FAILING_INIT_SRI: &str = "failing_init_test";
const NO_INIT_SRI: &str = "no_init_test";

/// Commands the room cell to publish its current temperature and returns the decoded degrees.
///
/// Commands are fire-and-forget, so the cell answers by publishing on the `temperature` event.
async fn read_temperature(ctx: &mut SwarmTestCtx, sri: &str) -> i32 {
    let payload = ctx
        .command_await_event(sri, "get_temperature", None, "temperature")
        .await;

    Temperature::from_payload(&payload)
        .expect("deser temperature")
        .degrees_celsius
}

/// When no instance state is pre-stored, deploying a cell with `#[init]` should
/// run init and produce the default state.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn deploy_without_stored_state_runs_init() {
    if !device_present() {
        return;
    }

    // Arrange — build room cell (registers class), start swarm, no pre-stored state
    let spawned = hil_swarm_test()
        .aot_cell(assert_ok!(build_aot_cell(ROOM_CELL)), ROOM_SRI)
        .spawn()
        .await;

    let _monitor = assert_ok!(flash_device());

    // Act — deploy the cell directly (no pre-stored state)
    let mut ctx = spawned.connect().await;

    // Assert — state should be the #[init] default (temperature=20)
    assert_eq!(
        read_temperature(&mut ctx, ROOM_SRI).await,
        20,
        "cell should use init default (20) when no stored state exists"
    );
}

/// Deploying with init arguments should seed the cell's state from those arguments instead of
/// running `#[init]`'s default path. The arguments ride along the deployment command payload.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn deploy_with_init_arguments_seeds_state() {
    if !device_present() {
        return;
    }

    // Arrange — deploy the room cell with an init payload seeding temperature=42 (its #[init]
    // default is 20).
    let seed = Temperature::new(42).to_payload().expect("serialize state");
    let spawned = hil_swarm_test()
        .aot_cell_with_payload(assert_ok!(build_aot_cell(ROOM_CELL)), ROOM_SRI, seed)
        .spawn()
        .await;

    let _monitor = assert_ok!(flash_device());

    // Act — deploy the cell (init consumes the payload)
    let mut ctx = spawned.connect().await;

    // Assert — state should be the seeded value, not the init default
    assert_eq!(
        read_temperature(&mut ctx, ROOM_SRI).await,
        42,
        "cell should use seeded init arguments (42), not init default (20)"
    );
}

/// When `#[init]` returns `Err(...)` and no stored state exists, deploy should
/// fail with a `DeploymentFailed` error carrying the init error message.
/// No registry entry should be left behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn deploy_without_stored_state_init_fails() {
    if !device_present() {
        return;
    }

    // Arrange — build failing-init cell (registers class), start swarm
    let spawned = hil_swarm_test()
        .aot_cell(
            assert_ok!(build_aot_cell(FAILING_INIT_CELL)),
            FAILING_INIT_SRI,
        )
        .spawn()
        .await;

    let _monitor = assert_ok!(flash_device());

    let mut ctx = spawned.connect_deferred().await;

    // Act — deploy the cell (no pre-stored state, init will fail)
    let err = assert_err!(ctx.try_load_cells().await);

    assert_init_failure(&err);
}

/// Redeploying a cell should skip init and preserve the saved state.
/// Deploy → mutate state → undeploy → redeploy → state is preserved.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn redeploy_preserves_state_and_skips_init() {
    if !device_present() {
        return;
    }

    // Arrange — build room cell, start swarm
    let spawned = hil_swarm_test()
        .aot_cell(assert_ok!(build_aot_cell(ROOM_CELL)), ROOM_SRI)
        .spawn()
        .await;

    let _monitor = assert_ok!(flash_device());

    // First deploy — init runs, temp=20
    let mut ctx = spawned.connect().await;

    // Mutate state to temp=99, then read it back so we know the write landed before undeploying.
    let payload = Temperature::new(99).to_payload().unwrap();
    ctx.command_send(ROOM_SRI, "set_temperature", Some(payload))
        .await;
    assert_eq!(
        read_temperature(&mut ctx, ROOM_SRI).await,
        99,
        "set_temperature should apply"
    );

    // Undeploy
    ctx.undeploy_cell(ROOM_SRI).await;

    // Act — redeploy same SRI
    ctx.queue_load(aot_class_name(ROOM_CELL), ROOM_SRI);
    ctx.load_cells().await;

    // Assert — state should be preserved (99), not re-initialized (20)
    assert_eq!(
        read_temperature(&mut ctx, ROOM_SRI).await,
        99,
        "redeployed cell should use saved state (99), not re-run init (20)"
    );
}

/// Redeploying after a failed init should retry init, not permanently disable it.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn redeploy_after_failed_init_retries_init() {
    if !device_present() {
        return;
    }

    // Arrange — build failing-init cell, start swarm
    let spawned = hil_swarm_test()
        .aot_cell(
            assert_ok!(build_aot_cell(FAILING_INIT_CELL)),
            FAILING_INIT_SRI,
        )
        .spawn()
        .await;

    let _monitor = assert_ok!(flash_device());

    let mut ctx = spawned.connect_deferred().await;

    // Act — first deploy fails. `try_load_cells` leaves the cell queued on failure, so the
    // second attempt below re-issues the very same deploy.
    assert_err!(ctx.try_load_cells().await);

    // Act — second deploy should also fail (init retries, not permanently disabled)
    let err = assert_err!(ctx.try_load_cells().await);

    // Assert — same init failure, proving init was retried
    assert_init_failure(&err);
}

/// When a cell has no `#[init]` and no stored state exists, it should deploy
/// with empty state (0 bytes) and still be able to receive commands.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn deploy_without_stored_state_no_init() {
    if !device_present() {
        return;
    }

    // Arrange — build no-init cell (registers class), start swarm
    let spawned = hil_swarm_test()
        .aot_cell(assert_ok!(build_aot_cell(NO_INIT_CELL)), NO_INIT_SRI)
        .spawn()
        .await;

    let _monitor = assert_ok!(flash_device());

    let mut ctx = spawned.connect_deferred().await;

    // Assert — the cell (no init, no stored state) deploys successfully. `ping` is a no-op with
    // no reply event, so a successful deploy plus an accepted fire-and-forget command is all we
    // can observe host-side.
    assert_ok!(
        ctx.try_load_cells().await,
        "cell with no init and no stored state should deploy"
    );
    ctx.command_send(NO_INIT_SRI, "ping", None).await;
}

/// Asserts `err` is a single-cell deployment failure the runtime reported as init-related.
fn assert_init_failure(err: &DeploymentError) {
    let DeploymentError::DeploymentFailed(failures) = err else {
        panic!("expected DeploymentFailed, got: {err:?}");
    };
    assert_eq!(1, failures.len(), "exactly one cell failure expected");
    let failure = &failures[0];
    assert!(
        matches!(&failure.kind, CellFailureKind::RuntimeReported(msg) if msg.contains("init")),
        "failure should be RuntimeReported with an init-related message, got: {:?}",
        failure.kind
    );
}
