//! Native bridge cell tests, driven on the standalone cell path (`deploy_http_bridge`/
//! `deploy_mqtt_bridge`/`deploy_modbus_bridge`/`undeploy_cell`), not through an app
//! deployment. These are the early-verification signal for the native bridge rebuild: a
//! deploy -> round-trip -> undeploy cycle for each bridge kind, plus the deploy/undeploy
//! edge cases.

use std::collections::BTreeMap;
use std::time::Duration;

use cell_protocol::{Gen, PlacementEntry, PlacementKind};
use claims::{assert_err, assert_none, assert_ok};
use myrmic_common::human_duration::NonZeroDuration;
use sorg_common::{
    BodyTemplate, DeploymentError, FenceOutcome, HttpBridgeApi, ModbusBridge, ModbusByteOrder,
    ModbusRegister, ModbusWritableRegister, MqttBridge, PlacementClaimOutcome, RequirementTags,
    WireHttpEndpoint, WireHttpRequestTemplate, WireHttpResponseVariant, WireModbusPoll,
    WireModbusWrite, WireMqttIngress, claim_placement, commit_placement,
};
use sorg_tests::{HttpMockHandle, ModbusMockHandle, swarm_config};

use crate::integration::{spawn_test_app_with_swarm, to_sri};

const HTTP_BRIDGE_SRI: &str = "standalone_http_bridge";
const MQTT_BRIDGE_SRI: &str = "standalone_mqtt_bridge";
const MODBUS_BRIDGE_SRI: &str = "standalone_modbus_bridge";

// The tests of one process share a swarm network (see `sorg_tests::swarm`), so each
// test deploys under names no concurrently running test uses.
const COLLIDING_BRIDGE_SRI: &str = "colliding_http_bridge";
const COLLIDING_APP: &str = "colliding_mqtt_bridge";

fn http_bridge_api(cell_name: &str, base_url: &str) -> HttpBridgeApi {
    HttpBridgeApi {
        cell_name: cell_name.to_owned(),
        base_url: base_url.to_owned(),
        endpoints: vec![WireHttpEndpoint {
            id: "fetch_data".to_owned(),
            request: WireHttpRequestTemplate {
                method: "POST".to_owned(),
                path: "/api/data".parse().unwrap(),
                query: BTreeMap::new(),
                headers: BTreeMap::new(),
                body: None,
                timeout_ms: None,
            },
            response: BTreeMap::from([(
                200,
                WireHttpResponseVariant {
                    headers: BTreeMap::new(),
                    body: Some(BodyTemplate::String("body".to_owned())),
                },
            )]),
        }],
    }
}

/// Deploy an HTTP bridge cell natively, drive its outbound HTTP call via a fire-and-forget
/// command, then undeploy and verify the cell is gone and stops calling out.
///
/// Commands no longer return a value (fire-and-forget) and egress bridges return `Ok(None)`,
/// so the bridge's work is observed at its external edge instead: the mock HTTP server
/// records the outbound POST the bridge makes.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn http_bridge_deploy_round_trip_undeploy() {
    // Arrange — orch + exec + db, plus an HTTP mock server
    let swarm = swarm_config!("cells/cells.jsonnet");
    let test_app = spawn_test_app_with_swarm(swarm).await;
    let sorg = sorg_client::Client::new(test_app.session().clone());

    let mock_server = HttpMockHandle::start().await;
    mock_server
        .expect_post("/api/data", b"native-bridge-ok".to_vec())
        .await;

    // Act I — deploy the bridge cell directly, no application wrapper
    assert_ok!(
        sorg.deploy_http_bridge(
            to_sri(HTTP_BRIDGE_SRI),
            http_bridge_api(HTTP_BRIDGE_SRI, mock_server.url()),
            RequirementTags::default(),
        )
        .await
    );

    // Assert I — registered natively, keyed by its own sri
    assert!(test_app.is_cell_registered(HTTP_BRIDGE_SRI).await);
    let entry = assert_ok!(sorg.get_placement(&to_sri(HTTP_BRIDGE_SRI)).await)
        .expect("http bridge cell should be registered after deploy");
    let PlacementKind::Bridge { sri } = &entry.kind else {
        panic!("expected Bridge placement, got {:?}", entry.kind);
    };
    assert_eq!(sri, &to_sri(HTTP_BRIDGE_SRI));

    // Act II — command the bridge to make its outbound call (fire-and-forget)
    test_app
        .command_send(HTTP_BRIDGE_SRI, "fetch_data", None)
        .await;

    // Assert II — the bridge actually called out to the mock server. Fire-and-forget is
    // async, so poll until the request lands.
    let mut hits = 0;
    for _ in 0..25 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        hits = mock_server.received_post_count("/api/data").await;
        if hits >= 1 {
            break;
        }
    }
    assert_eq!(
        hits, 1,
        "bridge should have made exactly one outbound POST to /api/data"
    );

    // Act III — undeploy the native bridge cell
    test_app.undeploy_cell(HTTP_BRIDGE_SRI).await;

    // Assert III — deregistered, and no longer calls out. Commanding the now-gone
    // bridge is rejected (it is no longer in the registry), and no further outbound
    // request reaches the mock server.
    assert!(!test_app.is_cell_registered(HTTP_BRIDGE_SRI).await);
    let err = assert_err!(
        test_app
            .try_command_send(HTTP_BRIDGE_SRI, "fetch_data", None)
            .await,
        "commanding an undeployed bridge should be rejected"
    )
    .to_string();
    assert!(
        err.contains("has no placement"),
        "expected a 'has no placement' error, got: {err}"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        mock_server.received_post_count("/api/data").await,
        1,
        "no further outbound POST should occur after the bridge is undeployed"
    );
}

/// Deploy an MQTT bridge cell natively, round-trip an ingress message from an external
/// broker into a cell event, then undeploy and verify ingress no longer reaches anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn mqtt_bridge_deploy_round_trip_undeploy() {
    const INGRESS_TOPIC: &str = "test/standalone/ingress";
    const INGRESS_EVENT: &str = "standalone_mqtt_ingress";

    // Arrange — orch + exec + db, plus an external-looking MQTT broker
    let swarm = swarm_config!("cells/cells.jsonnet");
    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let sorg = sorg_client::Client::new(test_app.session().clone());
    let (_broker_handle, _mqtt_client) = test_app.set_up_mqtt_broker().await;

    let bridge = MqttBridge {
        cell_name: MQTT_BRIDGE_SRI.to_owned(),
        broker: "mqtt://localhost:1883".to_owned(),
        ingress: vec![WireMqttIngress {
            id: INGRESS_EVENT.to_owned(),
            topic: INGRESS_TOPIC.to_owned(),
            qos: None,
            payload: BodyTemplate::String("payload".to_owned()),
        }],
        egress: vec![],
    };

    // Act I — deploy the bridge cell directly, no application wrapper
    assert_ok!(
        sorg.deploy_mqtt_bridge(to_sri(MQTT_BRIDGE_SRI), bridge, RequirementTags::default())
            .await
    );

    // Assert I — registered natively, keyed by its own sri
    assert!(test_app.is_cell_registered(MQTT_BRIDGE_SRI).await);
    let entry = assert_ok!(sorg.get_placement(&to_sri(MQTT_BRIDGE_SRI)).await)
        .expect("mqtt bridge cell should be registered after deploy");
    let PlacementKind::Bridge { sri } = &entry.kind else {
        panic!("expected Bridge placement, got {:?}", entry.kind);
    };
    assert_eq!(sri, &to_sri(MQTT_BRIDGE_SRI));

    // Act II — round-trip an ingress message from the external broker
    let mut event_queue = test_app.subscribe_cell_event(INGRESS_EVENT).await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    test_app
        .send_mqtt_msg(INGRESS_TOPIC, b"hello from native mqtt bridge".to_vec())
        .await;

    // Assert II — the bridge decoded the raw mqtt payload and published a cell event as
    // a JSON object keyed by the ingress payload's placeholder name.
    let received = assert_ok!(event_queue.receive().await);
    let payload = String::from_utf8(received).expect("event payload should be utf8 json");
    assert_eq!(payload, r#"{"payload":"hello from native mqtt bridge"}"#);

    // Act III — undeploy the native bridge cell
    test_app.undeploy_cell(MQTT_BRIDGE_SRI).await;

    // Assert III — deregistered, and ingress no longer produces an event
    assert!(!test_app.is_cell_registered(MQTT_BRIDGE_SRI).await);
    test_app
        .send_mqtt_msg(INGRESS_TOPIC, b"should not be forwarded".to_vec())
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_none!(assert_ok!(event_queue.try_receive().await));
}

/// Deploy a Modbus bridge cell natively against an in-process Modbus server, see a polled
/// value arrive as a cell event and a commanded value land in the server, then undeploy
/// and verify the bridge neither polls nor takes commands anymore.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn modbus_bridge_deploy_round_trip_undeploy() {
    const POLL_EVENT: &str = "standalone_modbus_level";
    const WRITE_COMMAND: &str = "standalone_modbus_setpoint";
    const LEVEL_ADDRESS: u16 = 10;
    const SETPOINT_ADDRESS: u16 = 20;

    // Arrange — orch + exec + db, plus a Modbus server with a value to poll
    let swarm = swarm_config!("cells/cells.jsonnet");
    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let sorg = sorg_client::Client::new(test_app.session().clone());

    let server = ModbusMockHandle::start().await;
    server.set_input_registers(LEVEL_ADDRESS, &[42]);

    let bridge = ModbusBridge {
        cell_name: MODBUS_BRIDGE_SRI.to_owned(),
        host: server.address().host,
        port: server.address().port,
        unit_id: None,
        timeout: None,
        poll: vec![WireModbusPoll {
            id: POLL_EVENT.to_owned(),
            register: ModbusRegister::Input,
            address: LEVEL_ADDRESS,
            value: "${u16:level}".parse().unwrap(),
            byte_order: ModbusByteOrder::Abcd,
            unit_id: None,
            interval: NonZeroDuration::new(std::time::Duration::from_millis(50)).unwrap(),
            on_change: true,
        }],
        read: vec![],
        write: vec![WireModbusWrite {
            id: WRITE_COMMAND.to_owned(),
            register: ModbusWritableRegister::Holding,
            address: SETPOINT_ADDRESS,
            value: "${u16:setpoint}".parse().unwrap(),
            byte_order: ModbusByteOrder::Abcd,
            unit_id: None,
        }],
    };

    // The bridge polls as soon as it is up, so listen before deploying.
    let mut event_queue = test_app.subscribe_cell_event(POLL_EVENT).await;
    tokio::time::sleep(Duration::from_millis(250)).await;

    // Act I — deploy the bridge cell directly, no application wrapper
    assert_ok!(
        sorg.deploy_modbus_bridge(
            to_sri(MODBUS_BRIDGE_SRI),
            bridge,
            RequirementTags::default()
        )
        .await
    );

    // Assert I — registered natively, keyed by its own sri
    assert!(test_app.is_cell_registered(MODBUS_BRIDGE_SRI).await);
    let entry = assert_ok!(sorg.get_placement(&to_sri(MODBUS_BRIDGE_SRI)).await)
        .expect("modbus bridge cell should be registered after deploy");
    let PlacementKind::Bridge { sri } = &entry.kind else {
        panic!("expected Bridge placement, got {:?}", entry.kind);
    };
    assert_eq!(sri, &to_sri(MODBUS_BRIDGE_SRI));

    // Assert II — the polled register arrives as a cell event, keyed by the field name
    // of its value template
    let received = assert_ok!(event_queue.receive().await);
    let payload = String::from_utf8(received).expect("event payload should be utf8 json");
    assert_eq!(payload, r#"{"level":42}"#);

    // Act III — command a write (fire-and-forget)
    test_app
        .command_send(
            MODBUS_BRIDGE_SRI,
            WRITE_COMMAND,
            Some(br#"{"setpoint": 215}"#.to_vec()),
        )
        .await;

    // Assert III — the value lands in the server's holding register. The command is
    // async, so poll until it does.
    let mut written = None;
    for _ in 0..25 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        written = server.holding_registers(SETPOINT_ADDRESS, 1);
        if written.is_some() {
            break;
        }
    }
    assert_eq!(written, Some(vec![215]));

    // Act IV — undeploy the native bridge cell
    test_app.undeploy_cell(MODBUS_BRIDGE_SRI).await;

    // Assert IV — deregistered, rejects commands, and a changed register no longer
    // produces an event
    assert!(!test_app.is_cell_registered(MODBUS_BRIDGE_SRI).await);
    let err = assert_err!(
        test_app
            .try_command_send(
                MODBUS_BRIDGE_SRI,
                WRITE_COMMAND,
                Some(br#"{"setpoint": 7}"#.to_vec()),
            )
            .await,
        "commanding an undeployed bridge should be rejected"
    )
    .to_string();
    assert!(
        err.contains("has no placement"),
        "expected a 'has no placement' error, got: {err}"
    );
    server.set_input_registers(LEVEL_ADDRESS, &[43]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_none!(assert_ok!(event_queue.try_receive().await));
}

/// Deploying a second bridge under an sri already claimed by a live bridge is rejected as
/// an explicit `DuplicateSri` deploy error — not a panic or a silent half-spawn — and the
/// first bridge is left untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn bridge_deploy_sri_collision() {
    let swarm = swarm_config!("cells/cells.jsonnet");
    let test_app = spawn_test_app_with_swarm(swarm).await;
    let sorg = sorg_client::Client::new(test_app.session().clone());

    let mock_server = HttpMockHandle::start().await;
    mock_server
        .expect_post("/api/data", b"first-bridge".to_vec())
        .await;

    assert_ok!(
        sorg.deploy_http_bridge(
            to_sri(COLLIDING_BRIDGE_SRI),
            http_bridge_api(COLLIDING_BRIDGE_SRI, mock_server.url()),
            RequirementTags::default(),
        )
        .await
    );
    assert!(test_app.is_cell_registered(COLLIDING_BRIDGE_SRI).await);

    // Act — attempt to deploy a second (mqtt) bridge under the same sri.
    // Its app name must differ from the first bridge's so the collision is
    // decided by the sri claim, not the app-name guard that precedes it.
    let colliding_bridge = MqttBridge {
        cell_name: COLLIDING_APP.to_owned(),
        broker: "mqtt://localhost:1883".to_owned(),
        ingress: vec![],
        egress: vec![],
    };
    let err = assert_err!(
        sorg.deploy_mqtt_bridge(
            to_sri(COLLIDING_BRIDGE_SRI),
            colliding_bridge,
            RequirementTags::default()
        )
        .await,
        "deploying a bridge under an sri already in use should be rejected"
    );

    // Assert — explicit DuplicateSri, not a panic; the first bridge is untouched
    let DeploymentError::DuplicateSri { sri } = &err else {
        panic!("expected DuplicateSri, got: {err:?}");
    };
    assert_eq!(*sri, to_sri(COLLIDING_BRIDGE_SRI));

    let entry = assert_ok!(sorg.get_placement(&to_sri(COLLIDING_BRIDGE_SRI)).await)
        .expect("first bridge should still be registered after the rejected collision");
    assert!(
        matches!(entry.kind, PlacementKind::Bridge { .. }),
        "first bridge's placement kind should be untouched, got {:?}",
        entry.kind
    );

    // Assert — the first bridge is still live: commanding it still fires its outbound call.
    // (Fire-and-forget, so poll the mock rather than await a response.)
    test_app
        .command_send(COLLIDING_BRIDGE_SRI, "fetch_data", None)
        .await;
    let mut hits = 0;
    for _ in 0..25 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        hits = mock_server.received_post_count("/api/data").await;
        if hits >= 1 {
            break;
        }
    }
    assert!(
        hits >= 1,
        "the first bridge should still call out after the rejected collision"
    );
}

/// A bridge whose external endpoint is unreachable fails deploy with an explicit error
/// — not a panic or a silent half-spawn — and leaves no cell registered behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn bridge_deploy_native_spawn_failure() {
    const UNREACHABLE_BRIDGE_SRI: &str = "unreachable_mqtt_bridge";

    let swarm = swarm_config!("cells/cells.jsonnet");
    let test_app = spawn_test_app_with_swarm(swarm).await;
    let sorg = sorg_client::Client::new(test_app.session().clone());

    // Nothing listens on this port, so connecting to the broker fails immediately.
    let bridge = MqttBridge {
        cell_name: UNREACHABLE_BRIDGE_SRI.to_owned(),
        broker: "mqtt://127.0.0.1:1".to_owned(),
        ingress: vec![],
        egress: vec![],
    };

    let err = assert_err!(
        sorg.deploy_mqtt_bridge(
            to_sri(UNREACHABLE_BRIDGE_SRI),
            bridge,
            RequirementTags::default()
        )
        .await,
        "deploying a bridge that cannot reach its broker should fail, not hang or panic"
    );
    assert!(
        matches!(err, DeploymentError::DeploymentFailed(_)),
        "expected DeploymentFailed, got: {err:?}"
    );

    // No half-spawned cell is left registered behind the failed deploy.
    assert!(!test_app.is_cell_registered(UNREACHABLE_BRIDGE_SRI).await);
}

/// Undeploying a bridge sri that was never deployed is a clean, defined error — not a
/// panic — the same as for any other cell kind.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn bridge_undeploy_when_absent() {
    let swarm = swarm_config!("cells/cells.jsonnet");
    let test_app = spawn_test_app_with_swarm(swarm).await;

    let err_msg = assert_err!(
        test_app.try_undeploy_cell("never_deployed_bridge").await,
        "undeploying a bridge sri that was never deployed should fail cleanly"
    )
    .to_string();
    assert!(
        err_msg.contains("not deployed"),
        "expected a 'not deployed' error, got: {err_msg}"
    );
}

/// Plants the placement row a bridge deploy leaves behind, with no bridge task behind
/// it. This is the state an orchestrator restart produces: the bridge registry lives
/// only in the process that spawned the bridge, the row outlives that process.
async fn plant_orphaned_bridge_row(session: &zenoh::Session, name: &str) {
    let sri = to_sri(name);
    let gen_id = Gen::from_parts(1, 1);
    let placeholder = PlacementEntry {
        sri,
        kind: PlacementKind::Placeholder,
        app: Some(name.to_owned()),
        gen_id,
    };
    assert_eq!(
        assert_ok!(claim_placement(session, placeholder).await),
        PlacementClaimOutcome::Claimed
    );
    let entry = PlacementEntry {
        sri,
        kind: PlacementKind::Bridge { sri },
        app: Some(name.to_owned()),
        gen_id,
    };
    assert_eq!(
        assert_ok!(commit_placement(session, entry).await),
        FenceOutcome::Applied
    );
}

/// A bridge row whose bridge is no longer running anywhere must still undeploy.
/// Releasing the row is the authoritative delete, and nothing else ever releases it.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn bridge_undeploy_releases_row_with_no_bridge_behind_it() {
    const ORPHAN_SRI: &str = "orphaned_bridge";
    let swarm = swarm_config!("cells/orch_only.jsonnet");
    let test_app = spawn_test_app_with_swarm(swarm).await;

    plant_orphaned_bridge_row(test_app.session(), ORPHAN_SRI).await;
    assert!(test_app.is_cell_registered(ORPHAN_SRI).await);

    test_app.undeploy_cell(ORPHAN_SRI).await;

    assert!(!test_app.is_cell_registered(ORPHAN_SRI).await);
}

/// The same orphan seen from the app: deleting the app must clear the row so the app
/// name is free again, and a fresh deploy under that name must then be admitted rather
/// than rejected as already deployed.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn app_delete_clears_orphaned_bridge_row_and_frees_the_app_name() {
    const ORPHAN_SRI: &str = "orphaned_app_bridge";
    let swarm = swarm_config!("cells/cells.jsonnet");
    let test_app = spawn_test_app_with_swarm(swarm).await;
    let sorg = sorg_client::Client::new(test_app.session().clone());

    plant_orphaned_bridge_row(test_app.session(), ORPHAN_SRI).await;

    assert_ok!(sorg.delete_application(ORPHAN_SRI).await);
    assert!(!test_app.is_cell_registered(ORPHAN_SRI).await);

    let mock_server = HttpMockHandle::start().await;
    assert_ok!(
        sorg.deploy_http_bridge(
            to_sri(ORPHAN_SRI),
            http_bridge_api(ORPHAN_SRI, mock_server.url()),
            RequirementTags::default(),
        )
        .await
    );
    assert!(test_app.is_cell_registered(ORPHAN_SRI).await);
}
