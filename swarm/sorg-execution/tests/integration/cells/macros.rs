//! Integration tests for cells implemented with the cell attribute macros.
//!
//! Migrated to the new model: commands are fire-and-forget, cross-cell replies
//! use `Callback` + `send_cmd_raw`, and results are observed via events the
//! cells publish (there is no synchronous command reply). Each host test
//! subscribes to the relevant event topic and asserts on the payload.
//!
//! Bare scalar payloads (a plain `i32`, `String`, ...) travel as JSON — the same
//! form an external caller such as the gateway would send — so this file encodes
//! and decodes them with `serde_json`. Wrapper message structs that opt into
//! `#[codec(Postcard)]` (e.g. `CountChanged`) still use postcard, and the
//! `Temperature` payload carries its own explicit postcard framing.

use claims::assert_ok;
use module_examples_common::Temperature;
use serde::Deserialize;
use sorg_tests::{build_and_register_cell_class, swarm_config};

use crate::integration::spawn_test_app_with_swarm;

const ROOM_SRI: &str = "room_cell";
const THERMOSTAT_SRI: &str = "thermostat_cell";

// --- Host-side mirror of the postcard `count_changed` event payload ---

#[derive(Deserialize)]
struct Counter {
    count: i32,
}

#[derive(Deserialize)]
struct CountChanged {
    counter: Counter,
}

// --- Room (single cell): default + set/get via the `temperature` event ---

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn room_returns_default_temperature() {
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class("../../tests/fixtures/cell-room-logic", "room", &swarm).await;

    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut temp_q = test_app.subscribe_cell_event("temperature").await;

    test_app
        .deploy_wasm_cell("room.wasm".to_owned(), ROOM_SRI.to_owned())
        .await;
    test_app
        .command_send(ROOM_SRI, "get_temperature", None)
        .await;

    let received = assert_ok!(temp_q.receive().await);
    let temp = Temperature::from_payload(&received).expect("deser");
    assert_eq!(temp.degrees_celsius, 20);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn room_set_and_get_temperature() {
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class("../../tests/fixtures/cell-room-logic", "room", &swarm).await;

    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut temp_q = test_app.subscribe_cell_event("temperature").await;

    test_app
        .deploy_wasm_cell("room.wasm".to_owned(), ROOM_SRI.to_owned())
        .await;

    let payload = Temperature::new(25).to_payload().unwrap();
    test_app
        .command_send(ROOM_SRI, "set_temperature", Some(payload))
        .await;
    test_app
        .command_send(ROOM_SRI, "get_temperature", None)
        .await;

    let received = assert_ok!(temp_q.receive().await);
    let temp = Temperature::from_payload(&received).expect("deser");
    assert_eq!(temp.degrees_celsius, 25);
}

// --- Thermostat delegates a set to the room cell (cross-cell fire-and-forget) ---

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn thermostat_sets_temperature() {
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class("../../tests/fixtures/cell-room-logic", "room", &swarm).await;
    build_and_register_cell_class(
        "../../tests/fixtures/cell-thermostat-logic",
        "thermostat",
        &swarm,
    )
    .await;
    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut confirm_q = test_app.subscribe_cell_event("room_temperature_set").await;

    test_app
        .deploy_wasm_cell("room.wasm".to_owned(), ROOM_SRI.to_owned())
        .await;
    test_app
        .deploy_wasm_cell("thermostat.wasm".to_owned(), THERMOSTAT_SRI.to_owned())
        .await;

    // `set_room_temperature` takes a bare `String`, carried as a JSON string.
    let payload = serde_json::to_vec("25").unwrap();
    test_app
        .command_send(THERMOSTAT_SRI, "set_room_temperature", Some(payload))
        .await;

    // The thermostat confirms the value it forwarded to the room cell.
    let received = assert_ok!(confirm_q.receive().await);
    let confirmed: String = serde_json::from_slice(&received).expect("deser confirm string");
    assert_eq!(confirmed, "25");
}

// --- Counter caller delegates increment + read to the counter cell via Callback ---

const COUNTER_SRI: &str = "counter_cell";
const COUNTER_CALLER_SRI: &str = "counter_caller_cell";

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn counter_caller_increments_via_api() {
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class("../../tests/fixtures/cell-counter-logic", "counter", &swarm)
        .await;
    build_and_register_cell_class(
        "../../tests/fixtures/cell-counter-caller-logic",
        "counter_caller",
        &swarm,
    )
    .await;
    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut value_q = test_app.subscribe_cell_event("counter_value").await;

    test_app
        .deploy_wasm_cell("counter.wasm".to_owned(), COUNTER_SRI.to_owned())
        .await;
    test_app
        .deploy_wasm_cell(
            "counter_caller.wasm".to_owned(),
            COUNTER_CALLER_SRI.to_owned(),
        )
        .await;

    // Increment by 5, then by 3; the caller publishes the accumulated count each
    // time as a bare `i32` (a JSON number).
    let payload1 = serde_json::to_vec(&5i32).unwrap();
    test_app
        .command_send(COUNTER_CALLER_SRI, "increment_and_get", Some(payload1))
        .await;
    let received1 = assert_ok!(value_q.receive().await);
    let count1: i32 = serde_json::from_slice(&received1).expect("deser count");
    assert_eq!(count1, 5);

    let payload2 = serde_json::to_vec(&3i32).unwrap();
    test_app
        .command_send(COUNTER_CALLER_SRI, "increment_and_get", Some(payload2))
        .await;
    let received2 = assert_ok!(value_q.receive().await);
    let count2: i32 = serde_json::from_slice(&received2).expect("deser count");
    assert_eq!(count2, 8);
}

// --- Nested domain types read back through a Callback reply ---

const NESTED_SRI: &str = "nested_cell";
const NESTED_CALLER_SRI: &str = "nested_caller_cell";

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn nested_caller_reads_nested_value() {
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class("../../tests/fixtures/cell-nested-logic", "nested", &swarm).await;
    build_and_register_cell_class(
        "../../tests/fixtures/cell-nested-caller-logic",
        "nested_caller",
        &swarm,
    )
    .await;
    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut value_q = test_app.subscribe_cell_event("measurement_value").await;

    test_app
        .deploy_wasm_cell("nested.wasm".to_owned(), NESTED_SRI.to_owned())
        .await;
    test_app
        .deploy_wasm_cell(
            "nested_caller.wasm".to_owned(),
            NESTED_CALLER_SRI.to_owned(),
        )
        .await;

    test_app
        .command_send(NESTED_CALLER_SRI, "read_value", None)
        .await;

    let received = assert_ok!(value_q.receive().await);
    let value: String = serde_json::from_slice(&received).expect("deser string");
    assert_eq!(value, "42");
}

// --- Event macro tests ---

const EVENT_PUB_SRI: &str = "event_pub_cell";

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn event_pub_publishes_event() {
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class(
        "../../tests/fixtures/cell-event-pub-logic",
        "event_pub",
        &swarm,
    )
    .await;
    let mut test_app = spawn_test_app_with_swarm(swarm).await;

    let mut event_queue = test_app.subscribe_cell_event("count_changed").await;

    test_app
        .deploy_wasm_cell("event_pub.wasm".to_owned(), EVENT_PUB_SRI.to_owned())
        .await;
    test_app.command_send(EVENT_PUB_SRI, "trigger", None).await;

    // `count_changed` is a `#[codec(Postcard)]` message struct.
    let received = assert_ok!(event_queue.receive().await);
    let count_changed: CountChanged = postcard::from_bytes(&received).expect("deser event payload");
    assert_eq!(count_changed.counter.count, 1);
}

const EVENT_SUB_SRI: &str = "event_sub_cell";

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn event_sub_forwards_event() {
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class(
        "../../tests/fixtures/cell-event-pub-logic",
        "event_pub",
        &swarm,
    )
    .await;
    build_and_register_cell_class(
        "../../tests/fixtures/cell-event-sub-logic",
        "event_sub",
        &swarm,
    )
    .await;
    let mut test_app = spawn_test_app_with_swarm(swarm).await;

    let mut forward_queue = test_app.subscribe_cell_event("count_forwarded").await;

    test_app
        .deploy_wasm_cell("event_pub.wasm".to_owned(), EVENT_PUB_SRI.to_owned())
        .await;
    test_app
        .deploy_wasm_cell("event_sub.wasm".to_owned(), EVENT_SUB_SRI.to_owned())
        .await;

    test_app.command_send(EVENT_PUB_SRI, "trigger", None).await;

    // The subscriber forwards the bare counter value as a JSON number.
    let received = assert_ok!(
        forward_queue.receive().await,
        "subscriber should have forwarded the count_forwarded event"
    );
    let forwarded: i32 = serde_json::from_slice(&received).expect("deser forwarded event payload");
    assert_eq!(forwarded, 1);
}

// --- Fire-and-forget tests ---

const FF_SENDER_SRI: &str = "ff_sender_cell";
const FF_RECEIVER_SRI: &str = "ff_receiver_cell";

/// The sender fires two commands to the receiver and then publishes its own
/// event. Because sends are non-blocking (and the receiver delays), the
/// sender's event arrives before the receiver's echoes.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn ff_macro_non_blocking_with_ordered_delivery() {
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class(
        "../../tests/fixtures/cell-ff-receiver-logic",
        "ff_receiver",
        &swarm,
    )
    .await;
    build_and_register_cell_class(
        "../../tests/fixtures/cell-ff-sender-logic",
        "ff_sender",
        &swarm,
    )
    .await;

    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut event_queue = test_app.subscribe_cell_event("ff_echo").await;

    test_app
        .deploy_wasm_cell("ff_receiver.wasm".to_owned(), FF_RECEIVER_SRI.to_owned())
        .await;
    test_app
        .deploy_wasm_cell("ff_sender.wasm".to_owned(), FF_SENDER_SRI.to_owned())
        .await;

    test_app.command_send(FF_SENDER_SRI, "trigger", None).await;

    let msg1 = assert_ok!(event_queue.receive().await);
    let msg1: String = serde_json::from_slice(&msg1).expect("deser ff_echo");
    assert_eq!(msg1, "sender_done");

    let msg2 = assert_ok!(event_queue.receive().await);
    let msg2: String = serde_json::from_slice(&msg2).expect("deser ff_echo");
    assert_eq!(msg2, "first");

    let msg3 = assert_ok!(event_queue.receive().await);
    let msg3: String = serde_json::from_slice(&msg3).expect("deser ff_echo");
    assert_eq!(msg3, "second");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn ff_macro_no_args_command() {
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class(
        "../../tests/fixtures/cell-ff-receiver-logic",
        "ff_receiver",
        &swarm,
    )
    .await;
    build_and_register_cell_class(
        "../../tests/fixtures/cell-ff-sender-logic",
        "ff_sender",
        &swarm,
    )
    .await;

    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut event_queue = test_app.subscribe_cell_event("ff_echo").await;

    test_app
        .deploy_wasm_cell("ff_receiver.wasm".to_owned(), FF_RECEIVER_SRI.to_owned())
        .await;
    test_app
        .deploy_wasm_cell("ff_sender.wasm".to_owned(), FF_SENDER_SRI.to_owned())
        .await;

    test_app
        .command_send(FF_SENDER_SRI, "trigger_ping", None)
        .await;

    let msg1 = assert_ok!(event_queue.receive().await);
    let msg1: String = serde_json::from_slice(&msg1).expect("deser ff_echo");
    assert_eq!(msg1, "ping_done");

    let msg2 = assert_ok!(event_queue.receive().await);
    let msg2: String = serde_json::from_slice(&msg2).expect("deser ff_echo");
    assert_eq!(msg2, "pong");
}

// --- Callback command tests ---

const CB_SENDER_SRI: &str = "cb_sender_cell";
const CB_RECEIVER_SRI: &str = "cb_receiver_cell";

/// Happy path: sender fires a callback command and publishes an immediate
/// event; the callback handler fires after the receiver completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn cb_macro_happy_path() {
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class(
        "../../tests/fixtures/cell-cb-receiver-logic",
        "cb_receiver",
        &swarm,
    )
    .await;
    build_and_register_cell_class(
        "../../tests/fixtures/cell-cb-sender-logic",
        "cb_sender",
        &swarm,
    )
    .await;

    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut event_queue = test_app.subscribe_cell_event("cb_echo").await;

    test_app
        .deploy_wasm_cell("cb_receiver.wasm".to_owned(), CB_RECEIVER_SRI.to_owned())
        .await;
    test_app
        .deploy_wasm_cell("cb_sender.wasm".to_owned(), CB_SENDER_SRI.to_owned())
        .await;

    test_app.command_send(CB_SENDER_SRI, "trigger", None).await;

    let msg1 = assert_ok!(event_queue.receive().await);
    let msg1: String = serde_json::from_slice(&msg1).expect("deser cb_echo");
    assert_eq!(msg1, "sender_immediate");

    let msg2 = assert_ok!(event_queue.receive().await);
    let msg2: String = serde_json::from_slice(&msg2).expect("deser cb_echo");
    assert_eq!(msg2, "receiver_done");

    let msg3 = assert_ok!(event_queue.receive().await);
    let msg3: String = serde_json::from_slice(&msg3).expect("deser cb_echo");
    assert_eq!(msg3, "callback_done");
}

/// No-args void-return callback path.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn cb_macro_no_args_void_return() {
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class(
        "../../tests/fixtures/cell-cb-receiver-logic",
        "cb_receiver",
        &swarm,
    )
    .await;
    build_and_register_cell_class(
        "../../tests/fixtures/cell-cb-sender-logic",
        "cb_sender",
        &swarm,
    )
    .await;

    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut event_queue = test_app.subscribe_cell_event("cb_echo").await;

    test_app
        .deploy_wasm_cell("cb_receiver.wasm".to_owned(), CB_RECEIVER_SRI.to_owned())
        .await;
    test_app
        .deploy_wasm_cell("cb_sender.wasm".to_owned(), CB_SENDER_SRI.to_owned())
        .await;

    test_app
        .command_send(CB_SENDER_SRI, "trigger_ping", None)
        .await;

    let msg1 = assert_ok!(event_queue.receive().await);
    let msg1: String = serde_json::from_slice(&msg1).expect("deser cb_echo");
    assert_eq!(msg1, "sender_immediate");

    let msg2 = assert_ok!(event_queue.receive().await);
    let msg2: String = serde_json::from_slice(&msg2).expect("deser cb_echo");
    assert_eq!(msg2, "pong");

    let msg3 = assert_ok!(event_queue.receive().await);
    let msg3: String = serde_json::from_slice(&msg3).expect("deser cb_echo");
    assert_eq!(msg3, "ping_done");
}

// --- PARKED / DELETED (new-model) ---
//
// DELETED: room_manual_* / thermostat_manual_* (the low-level manual state API
// they mirrored is gone); cb_macro_error_handler & cb_macro_correlation (the
// callback error channel and correlation attachment were removed); and
// event_sub_forwards_event_when_deployed_from_stored_state (pre-storing an
// instance via `create_instance` — the single-blob stored-state model — is gone).
//
// PARKED: heater_checks_room_temperature — a deep synchronous heater/room
// callback chain with no direct fire-and-forget equivalent. Revisit.
