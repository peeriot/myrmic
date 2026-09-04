//! Tests on Cell Commands (manually mirrors tests in
//! `swarm/sorg-execution/tests/integration/cells/commands.rs`)
//!
//! The `cell-ff-sender-logic` cell, on `trigger`, sends two `accept` commands to
//! `cell-ff-receiver-logic` and then publishes `sender_done` on the shared `ff_echo` event; the
//! receiver echoes each payload (`first`, `second`) back on the same event after a short delay.

use cell_protocol::Sri;
use claims::assert_ok;
use serde::Deserialize;

use crate::integration::{
    TAG_LINUX,
    aot::{build_aot_cell, build_wasm_cell},
    device_present,
    espflash::flash_device,
    hil_swarm_test,
};

const SENDER_CELL: &str = "cell-ff-sender-logic";
const RECEIVER_CELL: &str = "cell-ff-receiver-logic";

const SENDER_SRI: &str = "ff_sender_cell";
// Must match the `RECEIVER` SRI hard-coded in `cell-ff-sender-logic`.
const RECEIVER_SRI: &str = "ff_receiver_cell";
const FF_EVENT: &str = "ff_echo";

/// Event the receiver publishes carrying the SRI of whoever sent it the `accept` command.
const SENDER_EVENT: &str = "ff_sender";

/// Mirrors the `FfSender` payload the receiver publishes on the `ff_sender` event: the `md.sender`
/// stamped on the incoming `accept` command.
#[derive(Deserialize)]
struct FfSender {
    sender: Sri,
}

/// Decodes the string an `ff_echo` event carries. The cells publish a bare `String` payload, which
/// the SDK carries on the wire as a JSON string.
fn echo_payload(bytes: &[u8]) -> String {
    serde_json::from_slice::<String>(bytes).expect("ff_echo payload should decode as a JSON string")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn ff_command_non_blocking_with_ordered_delivery_emb_recv() {
    if !device_present() {
        return;
    }

    // Receiver on the device, sender on the host.
    let spawned = hil_swarm_test()
        .aot_cell(assert_ok!(build_aot_cell(RECEIVER_CELL)), RECEIVER_SRI)
        .wasm_artifact_on(
            assert_ok!(build_wasm_cell(SENDER_CELL)),
            SENDER_SRI,
            &[TAG_LINUX],
        )
        .spawn()
        .await;

    let _monitor = assert_ok!(flash_device());

    // Subscribe to the shared event topic before loading any cells
    let mut ctx = spawned.connect_deferred().await;
    let mut event_queue = ctx.subscribe_cell_event(FF_EVENT).await;
    ctx.load_cells().await;

    // Act - trigger the sender
    ctx.command_send(SENDER_SRI, "trigger", None).await;

    // Assert - event ordering: sender_done must arrive before the receiver's echoed payloads.
    //  The sender cell is on Linux so we can assume the ordering of events.
    tracing::info!("Waiting for sender_done event");
    let msg = assert_ok!(event_queue.receive().await);
    assert_eq!("sender_done", echo_payload(&msg));
    tracing::info!("Waiting for first event");
    let msg = assert_ok!(event_queue.receive().await);
    assert_eq!("first", echo_payload(&msg));
    tracing::info!("Waiting for second event");
    let msg = assert_ok!(event_queue.receive().await);
    assert_eq!("second", echo_payload(&msg));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn ff_command_non_blocking_with_ordered_delivery_emb_send() {
    if !device_present() {
        return;
    }

    // Sender on the device, receiver on the host.
    let spawned = hil_swarm_test()
        .wasm_artifact_on(
            assert_ok!(build_wasm_cell(RECEIVER_CELL)),
            RECEIVER_SRI,
            &[TAG_LINUX],
        )
        .aot_cell(assert_ok!(build_aot_cell(SENDER_CELL)), SENDER_SRI)
        .spawn()
        .await;

    let _monitor = assert_ok!(flash_device());

    // Subscribe to the shared event topic before loading any cells
    let mut ctx = spawned.connect_deferred().await;
    let mut event_queue = ctx.subscribe_cell_event(FF_EVENT).await;
    ctx.load_cells().await;

    // Act - trigger the sender
    ctx.command_send(SENDER_SRI, "trigger", None).await;

    // Assert - the sender cell is on embedded, so no ordering assumptions can be made; just
    // confirm all three payloads arrive.
    tracing::info!("Waiting for events");
    let mut events: Vec<String> = Vec::new();
    for _ in 0..3 {
        events.push(echo_payload(&assert_ok!(event_queue.receive().await)));
    }

    assert!(events.contains(&"sender_done".to_string()));
    assert!(events.contains(&"first".to_string()));
    assert!(events.contains(&"second".to_string()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn command_from_embedded_stamps_sender_sri() {
    if !device_present() {
        return;
    }

    // Sender on the device, receiver on the host: verifies the device stamps its own SRI on the
    // command it sends.
    let spawned = hil_swarm_test()
        .aot_cell(assert_ok!(build_aot_cell(SENDER_CELL)), SENDER_SRI)
        .wasm_artifact_on(
            assert_ok!(build_wasm_cell(RECEIVER_CELL)),
            RECEIVER_SRI,
            &[TAG_LINUX],
        )
        .spawn()
        .await;

    let _monitor = assert_ok!(flash_device());

    let mut ctx = spawned.connect_deferred().await;
    let mut q = ctx.subscribe_cell_event(SENDER_EVENT).await;
    ctx.load_cells().await;

    ctx.command_send(SENDER_SRI, "trigger", None).await;

    let msg = assert_ok!(q.receive().await);
    let ffs: FfSender = postcard::from_bytes(&msg).expect("decode FfSender");
    assert_eq!(
        ffs.sender,
        Sri::from_target(SENDER_SRI).expect("valid sri"),
        "device must stamp its own SRI as the command sender"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn command_to_embedded_delivers_sender() {
    if !device_present() {
        return;
    }

    // Sender on the host, receiver on the device: verifies the embedded receiver sees the sender's
    // SRI in `md.sender`.
    let spawned = hil_swarm_test()
        .wasm_artifact_on(
            assert_ok!(build_wasm_cell(SENDER_CELL)),
            SENDER_SRI,
            &[TAG_LINUX],
        )
        .aot_cell(assert_ok!(build_aot_cell(RECEIVER_CELL)), RECEIVER_SRI)
        .spawn()
        .await;

    let _monitor = assert_ok!(flash_device());

    let mut ctx = spawned.connect_deferred().await;
    let mut q = ctx.subscribe_cell_event(SENDER_EVENT).await;
    ctx.load_cells().await;

    ctx.command_send(SENDER_SRI, "trigger", None).await;

    let msg = assert_ok!(q.receive().await);
    let ffs: FfSender = postcard::from_bytes(&msg).expect("decode FfSender");
    assert_eq!(
        ffs.sender,
        Sri::from_target(SENDER_SRI).expect("valid sri"),
        "embedded receiver must see the command sender's SRI in md.sender"
    );
}
