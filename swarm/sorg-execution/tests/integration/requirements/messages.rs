use std::time::Duration;

use cell_protocol::Sri;
use claims::assert_ok;
use sorg_tests::{build_and_register_cell_class, enable_test_logging, swarm_config};

use crate::integration::spawn_test_app_with_swarm;

// --- Scenario 1 constants ---
const COUNTER_CELL_NAME: &str = "counter_delay";
const EVENT_PROCESSED: &str = "processed";
const SRI_A: &str = "cell_a";
const SRI_B: &str = "cell_b";

// --- Scenario 2 constants ---
const SEQ_MAIN_NAME: &str = "seq_main";
const SEQ_HELPER_NAME: &str = "seq_helper";
const MAIN_SRI: &str = "main_cell";
const HELPER_SRI: &str = "helper_cell";
const EVENT_COUNTER_READ: &str = "counter_read";

/// The cells publish `"{md.id}:{count}"`, and `md.id` renders as the cell's SRI
/// (a UUID), not its human name. Derive the expected string from the same SRI
/// the cell was deployed under so host and guest agree.
fn expected_event(cell_name: &str, count: u32) -> String {
    format!("{}:{}", Sri::from_target(cell_name).unwrap(), count)
}

/// Requirement 1: Per-cell independent queuing (#376)
///
/// Messages to different cells are queued and processed independently.
/// Cell A (no delay) processes two messages before cell B (50ms delay)
/// finishes its first.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn per_cell_independent_queuing() {
    // Arrange — build the cell module and deploy two instances
    enable_test_logging("warn");
    let swarm = swarm_config!("requirements/messages/swarm.jsonnet");
    build_and_register_cell_class(
        "../../tests/fixtures/cell-counter-delay-logic",
        COUNTER_CELL_NAME,
        &swarm,
    )
    .await;

    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut event_queue = test_app.subscribe_cell_event(EVENT_PROCESSED).await;

    test_app
        .deploy_wasm_cell(format!("{COUNTER_CELL_NAME}.wasm"), SRI_A)
        .await;
    test_app
        .deploy_wasm_cell(format!("{COUNTER_CELL_NAME}.wasm"), SRI_B)
        .await;

    let payload_0 = postcard::to_allocvec(&0u32).unwrap();
    let payload_100 = postcard::to_allocvec(&100u32).unwrap();

    // Act — send messages with staggered timing
    // t=0: send to A(0) and B(100)
    test_app
        .command_send(SRI_A, "process", Some(payload_0.clone()))
        .await;
    test_app
        .command_send(SRI_B, "process", Some(payload_100.clone()))
        .await;

    // t=10ms: send to A(0)
    tokio::time::sleep(Duration::from_millis(10)).await;
    test_app
        .command_send(SRI_A, "process", Some(payload_0.clone()))
        .await;

    // t=250ms: send to A(0) and B(200)
    tokio::time::sleep(Duration::from_millis(240)).await;
    test_app
        .command_send(SRI_A, "process", Some(payload_0.clone()))
        .await;
    test_app
        .command_send(SRI_B, "process", Some(payload_100.clone()))
        .await;

    // Assert — events arrive in order proving A and B are processed independently
    let expected = [
        expected_event(SRI_A, 0),
        expected_event(SRI_A, 1),
        expected_event(SRI_B, 0),
        expected_event(SRI_A, 2),
        expected_event(SRI_B, 1),
    ];

    let mut received_events = Vec::new();
    for _ in 0..expected.len() {
        let received = assert_ok!(event_queue.receive().await);
        let received_str = String::from_utf8(received).expect("event payload should be utf-8");
        dbg!(&received_str);
        received_events.push(received_str);
    }
    assert_eq!(received_events, expected);
}

/// Requirement 2: Sequential processing per cell (#376)
///
/// Messages to the same cell are processed one at a time, in order. Command A
/// fires a helper call then increments a counter; command B reads the counter.
/// Sending B, A, B proves A's state change is visible to the subsequent B.
///
/// NOTE: the original test relied on a *synchronous* cross-cell call to block
/// A until the helper finished. Synchronous sends were removed, so `call_helper`
/// is now fire-and-forget; this test now verifies per-cell FIFO ordering only.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn sequential_processing_per_cell() {
    // Arrange — build both cell modules and deploy them
    enable_test_logging("warn");
    let swarm = swarm_config!("requirements/messages/swarm.jsonnet");
    build_and_register_cell_class(
        "../../tests/fixtures/cell-seq-helper-logic",
        SEQ_HELPER_NAME,
        &swarm,
    )
    .await;
    build_and_register_cell_class(
        "../../tests/fixtures/cell-seq-main-logic",
        SEQ_MAIN_NAME,
        &swarm,
    )
    .await;

    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut event_queue = test_app.subscribe_cell_event(EVENT_COUNTER_READ).await;

    test_app
        .deploy_wasm_cell(format!("{SEQ_HELPER_NAME}.wasm"), HELPER_SRI)
        .await;
    test_app
        .deploy_wasm_cell(format!("{SEQ_MAIN_NAME}.wasm"), MAIN_SRI)
        .await;

    // `call_helper` takes a bare `u32`, carried on the wire as a JSON number.
    let payload_50 = serde_json::to_vec(&50u32).unwrap();

    // Act — send B, A, B (all fire-and-forget to main cell)
    test_app.command_send(MAIN_SRI, "read_counter", None).await;
    test_app
        .command_send(MAIN_SRI, "call_helper", Some(payload_50))
        .await;
    test_app.command_send(MAIN_SRI, "read_counter", None).await;

    // Assert — events arrive in order: counter=0 then counter=1
    let expected = [expected_event(MAIN_SRI, 0), expected_event(MAIN_SRI, 1)];

    let mut received_events = Vec::new();
    for _ in 0..expected.len() {
        let received = assert_ok!(event_queue.receive().await);
        let received_str = String::from_utf8(received).expect("event payload should be utf-8");
        dbg!(&received_str);
        received_events.push(received_str);
    }
    assert_eq!(received_events, expected);
}

/// Requirement 3: Cross-cell parallelism (#376)
///
/// A long-running handler in one cell does not prevent other cells from
/// making progress. Cell B (50ms delay) receives a message first, then
/// cell A (no delay) receives one 10ms later. A's event must arrive
/// before B's.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn cross_cell_parallelism() {
    // Arrange — build the cell module and deploy two instances
    enable_test_logging("warn");
    let swarm = swarm_config!("requirements/messages/swarm.jsonnet");
    build_and_register_cell_class(
        "../../tests/fixtures/cell-counter-delay-logic",
        COUNTER_CELL_NAME,
        &swarm,
    )
    .await;

    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut event_queue = test_app.subscribe_cell_event(EVENT_PROCESSED).await;

    test_app
        .deploy_wasm_cell(format!("{COUNTER_CELL_NAME}.wasm"), SRI_A)
        .await;
    test_app
        .deploy_wasm_cell(format!("{COUNTER_CELL_NAME}.wasm"), SRI_B)
        .await;

    let payload_0 = postcard::to_allocvec(&0u32).unwrap();
    let payload_50 = postcard::to_allocvec(&50u32).unwrap();

    // Act — send to B first, then to A after a short gap
    test_app
        .command_send(SRI_B, "process", Some(payload_50))
        .await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    test_app
        .command_send(SRI_A, "process", Some(payload_0))
        .await;

    // Assert — A's event arrives before B's despite being sent later
    let first = assert_ok!(event_queue.receive().await);
    let first = String::from_utf8(first).expect("event payload should be utf-8");
    let second = assert_ok!(event_queue.receive().await);
    let second = String::from_utf8(second).expect("event payload should be utf-8");

    assert_eq!(first, expected_event(SRI_A, 0));
    assert_eq!(second, expected_event(SRI_B, 0));
}
