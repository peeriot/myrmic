//! Gateway routes and assets are a cell's own resources: declared in its
//! `#[init]`, they are there after a successful deploy and gone once the cell
//! is rolled back as part of a failed batch. A gateway in front of the cell
//! delivers its replies.

use std::sync::Arc;
use std::time::{Duration, Instant};

use cell_protocol::BlobHash;
use claims::{assert_err, assert_ok};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Message;
use sorg_common::gateway_config::{list_cell_assets, list_gateway_routes};
use sorg_common::{CellConfig, CellDeployment, DeployRequest, DeploymentError};
use sorg_tests::{register_fixture_class, swarm_config};
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

fn wasm_cell(sri: &str, class: BlobHash) -> CellDeployment {
    CellDeployment::new(to_sri(sri), CellConfig::Wasm { class })
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
    register_fixture_class(GATEWAY_LOGIC, GATEWAY_CLASS, &swarm).await;
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
    register_fixture_class(GATEWAY_LOGIC, GATEWAY_CLASS, &swarm).await;
    let test_app = spawn_test_app_with_swarm(swarm).await;
    let failing = test_app
        .register_raw_class(FAILING_CLASS, vec![0xFF, 0xFF])
        .await;
    let gateway = test_app.class_id("gateway_mount.wasm").await;
    let sorg = sorg_client::Client::new(test_app.session().clone());

    let err = assert_err!(
        sorg.deploy_cells(DeployRequest::new(vec![
            wasm_cell(GATEWAY_SRI, gateway),
            wasm_cell(FAILING_SRI, failing),
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

// A real gateway in front of the deployed cell: each kind of reply the cell
// gives reaches the client.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gateway_delivers_each_kind_of_reply() {
    let swarm = swarm_config!("cells/cells.jsonnet");
    register_fixture_class(GATEWAY_LOGIC, GATEWAY_CLASS, &swarm).await;
    let test_app = spawn_test_app_with_swarm(swarm).await;
    test_app
        .deploy_wasm_cell("gateway_mount.wasm", GATEWAY_SRI)
        .await;

    let port = assert_ok!(std::net::TcpListener::bind("[::1]:0"))
        .local_addr()
        .unwrap()
        .port();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let gateway = tokio::spawn(swarm_gateway::run(
        swarm_gateway::Config {
            port: Some(port),
            ..Default::default()
        },
        test_app.session().clone(),
        Arc::new(Notify::new()),
        async move {
            let _ = stopped.await;
        },
    ));

    let base = format!("http://[::1]:{port}{MOUNT}");
    let client = assert_ok!(
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
    );

    // An asset, once the gateway has discovered the route.
    let index = wait_for_ok(&client, &format!("{base}/")).await;
    assert_eq!(index, "<h1>gateway mount test</h1>");

    // The bare mount redirects, so relative links resolve under it.
    let redirect = assert_ok!(client.get(&base).send().await);
    assert_eq!(redirect.status(), 307);
    assert_eq!(redirect.headers()["location"], format!("{MOUNT}/"));

    // A response the cell built.
    let echo = assert_ok!(client.post(format!("{base}/echo")).body("ping").send().await);
    assert_eq!(echo.status(), 200);
    assert_eq!(assert_ok!(echo.text().await), "ping");

    // An upgrade for a request that didn't ask for one.
    let no_upgrade = assert_ok!(client.get(format!("{base}/ws")).send().await);
    assert_eq!(no_upgrade.status(), 426);

    // An upgrade, echoing text and binary frames.
    let (mut socket, _) =
        assert_ok!(tokio_tungstenite::connect_async(format!("ws://[::1]:{port}{MOUNT}/ws")).await);
    for frame in [Message::text("hi"), Message::binary(vec![1, 2, 3])] {
        assert_ok!(socket.send(frame.clone()).await);
        assert_eq!(next_data_frame(&mut socket).await, frame);
    }
    assert_ok!(socket.close(None).await);

    let _ = stop.send(());
    assert_ok!(assert_ok!(gateway.await));
}

/// GETs `url` until it answers 200, returning the body.
async fn wait_for_ok(client: &reqwest::Client, url: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let response = client.get(url).send().await;
        if let Ok(response) = response
            && response.status() == 200
        {
            return assert_ok!(response.text().await);
        }
        assert!(Instant::now() < deadline, "{url} never answered 200");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn next_data_frame<S>(socket: &mut S) -> Message
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), socket.next()).await;
        let frame = assert_ok!(assert_ok!(frame).expect("socket closed early"));
        if frame.is_text() || frame.is_binary() {
            return frame;
        }
    }
}
