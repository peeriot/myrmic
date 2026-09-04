//! Gateway routes and assets are a cell's own resources: declared in its
//! `#[init]`, they are there after a successful deploy and gone once the cell
//! is rolled back as part of a failed batch.

use claims::{assert_err, assert_ok};
use sorg_common::gateway_config::{list_cell_assets, list_gateway_routes};
use sorg_common::{CellConfig, CellDeployment, DeployRequest, DeploymentError};
use sorg_tests::{build_and_register_cell_class, swarm_config};
use zenoh::Session;

use crate::integration::{spawn_test_app_with_swarm, to_sri};

const GATEWAY_LOGIC: &str = "../../tests/fixtures/cell-gateway-mount-logic";
const GATEWAY_CLASS: &str = "gateway_mount";
const GATEWAY_SRI: &str = "gateway_cell";
/// Garbage wasm: the exec fails at load time, which fails the whole batch.
const FAILING_CLASS: &str = "failing_cell";
const FAILING_SRI: &str = "failing_cell";

/// What the fixture declares in its `#[init]`.
const MOUNT: &str = "/gateway-mount-test";
const INDEX: &str = "/index.html";

fn wasm_cell(sri: &str, class: &str) -> CellDeployment {
    CellDeployment::new(
        to_sri(sri),
        CellConfig::Wasm {
            class: class.to_owned(),
        },
    )
}

async fn routes_owned_by(session: &Session, sri: &str) -> Vec<String> {
    let owner = to_sri(sri);
    assert_ok!(list_gateway_routes(session).await)
        .into_iter()
        .filter(|route| route.owner == owner)
        .map(|route| route.mount)
        .collect()
}

async fn assets_owned_by(session: &Session, sri: &str) -> Vec<String> {
    assert_ok!(list_cell_assets(session, &to_sri(sri)).await)
}

// A cell that mounts a route and uploads an asset in `#[init]` owns both once
// deployed — the positive control for the rollback test below.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn deploy_keeps_gateway_resources() {
    let swarm = swarm_config!("cells/cells.jsonnet");
    build_and_register_cell_class(GATEWAY_LOGIC, GATEWAY_CLASS, &swarm).await;
    let test_app = spawn_test_app_with_swarm(swarm).await;

    test_app
        .deploy_wasm_cell("gateway_mount.wasm", GATEWAY_SRI)
        .await;

    assert!(test_app.is_cell_registered(GATEWAY_SRI).await);
    assert_eq!(
        routes_owned_by(test_app.session(), GATEWAY_SRI).await,
        vec![MOUNT.to_owned()]
    );
    assert_eq!(
        assets_owned_by(test_app.session(), GATEWAY_SRI).await,
        vec![INDEX.to_owned()]
    );
}

// A batch where one cell fails rolls the others back — including the routes
// and assets a rolled-back cell already declared in its `#[init]`.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn rollback_releases_gateway_resources() {
    let swarm = swarm_config!("cells/cells.jsonnet");
    build_and_register_cell_class(GATEWAY_LOGIC, GATEWAY_CLASS, &swarm).await;
    let test_app = spawn_test_app_with_swarm(swarm).await;
    test_app
        .register_raw_class(FAILING_CLASS, vec![0xFF, 0xFF])
        .await;
    let sorg = sorg_client::Client::new(test_app.session().clone());

    let err = assert_err!(
        sorg.deploy_cells(DeployRequest::new(vec![
            wasm_cell(GATEWAY_SRI, "gateway_mount.wasm"),
            wasm_cell(FAILING_SRI, FAILING_CLASS),
        ]))
        .await,
        "deploy should fail when the garbage cell fails to load"
    );
    assert!(
        matches!(err, DeploymentError::DeploymentFailed(_)),
        "expected DeploymentFailed, got: {err:?}"
    );

    assert!(!test_app.is_cell_registered(GATEWAY_SRI).await);
    assert!(!test_app.is_cell_registered(FAILING_SRI).await);
    assert!(
        routes_owned_by(test_app.session(), GATEWAY_SRI)
            .await
            .is_empty(),
        "rollback should release the rolled-back cell's routes"
    );
    assert!(
        assets_owned_by(test_app.session(), GATEWAY_SRI)
            .await
            .is_empty(),
        "rollback should purge the rolled-back cell's assets"
    );
}
