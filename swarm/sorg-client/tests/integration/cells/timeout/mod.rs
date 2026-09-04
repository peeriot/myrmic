use std::time::Duration;

use cell_protocol::Sri;
use claims::{assert_err, assert_ok};
use sorg_client::{Client as SorgClient, Config};
use sorg_common::RequirementTags;
use sorg_tests::{build_and_register_cell_class, swarm_config};

use crate::integration::spawn_full_test_app_with_swarm;

const CELL_SRI: &str = "slow-init-cell";
const CELL_CLASS: &str = "slow_init";

/// The slow-init cell waits 12s during init — longer than Zenoh's default
/// 10s query timeout, but shorter than the generous client timeout we
/// configure in the success test.
const SLOW_INIT_DURATION: Duration = Duration::from_secs(12);

fn sorg_client_with_timeout(session: &zenoh::Session, timeout: Duration) -> SorgClient {
    let mut config = Config::default();
    config.set_query_timeout(timeout);
    SorgClient::new_with_config(session.clone(), config)
}

async fn spawn_test_app_with_slow_init_cell() -> (sorg_tests::TestApp, zenoh::Session) {
    let swarm = swarm_config!("full_slow_init.jsonnet");
    build_and_register_cell_class(
        "../../tests/fixtures/slow-init-cell-logic",
        CELL_CLASS,
        &swarm,
    )
    .await;
    let test_app = spawn_full_test_app_with_swarm(swarm).await;
    let session = test_app.session().clone();
    (test_app, session)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn deploy_succeeds_with_sufficient_timeout() {
    let (_test_app, session) = spawn_test_app_with_slow_init_cell().await;
    let sorg = sorg_client_with_timeout(&session, SLOW_INIT_DURATION + Duration::from_secs(8));

    let sri = Sri::from_target(CELL_SRI).unwrap();
    let class_name = format!("{CELL_CLASS}.wasm");

    // Act + Assert — deploy succeeds because the client timeout (20s) is
    // propagated to the Zenoh query, overriding the 10s default.
    assert_ok!(
        sorg.deploy_wasm_cell(sri, &class_name, RequirementTags::default())
            .await,
        "deploy should succeed when client timeout exceeds init duration"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn deploy_timeout_reports_clear_error() {
    let (_test_app, session) = spawn_test_app_with_slow_init_cell().await;
    let sorg = sorg_client_with_timeout(&session, Duration::from_secs(5));

    let sri = Sri::from_target(CELL_SRI).unwrap();
    let class_name = format!("{CELL_CLASS}.wasm");

    // Act — deploy with a timeout shorter than the cell's init
    let result = sorg
        .deploy_wasm_cell(sri, &class_name, RequirementTags::default())
        .await;

    // Assert — error should be a query timeout
    let err = assert_err!(result, "deploy should fail when timeout is too short");
    assert_eq!(
        err,
        sorg_common::DeploymentError::QueryTimeout,
        "expected QueryTimeout, got: {err}"
    );
}
