//! Tests on Event (manually mirrors tests in
//! `swarm/sorg-execution/tests/integration/cells/events.rs`)

use cell_protocol::Sri;
use claims::{assert_matches, assert_ok};

use crate::integration::{
    TAG_LINUX,
    aot::{build_aot_cell, build_wasm_cell},
    device_present,
    espflash::flash_device,
    hil_swarm_test,
};

const EVENT_ONE: &str = "my_event";
const EVENT_TWO: &str = "other_event";
const EVENT_THREE: &str = "third_event";
/// Event `event_subscribe` republishes the incoming event's sender SRI (raw 16 bytes) on.
const SENDER_EVENT: &str = "evt_sender";

const CMD_PUBLISH: &str = "publish_event";

const SRI_PUB: &str = "publisher";
const SRI_SUB: &str = "subscriber";
const SRI_SUB_B: &str = "subscriber_b";

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn cell_publishes_event() {
    if !device_present() {
        return;
    }

    let spawned = hil_swarm_test()
        .aot_cell(assert_ok!(build_aot_cell("event_publish")), SRI_PUB)
        .spawn()
        .await;

    let _monitor = assert_ok!(flash_device());

    let mut ctx = spawned.connect_deferred().await;
    let mut e_queue = ctx.subscribe_cell_event(EVENT_ONE).await;
    ctx.load_cells().await;

    // Act - command the cell to publish
    ctx.command_send(SRI_PUB, CMD_PUBLISH, None).await;

    // Assert - we expect to receive the event
    let expected = b"pub_payload";

    assert_matches!(e_queue.receive().await, Ok(ev) if ev == *expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn cell_subscribes_to_event() {
    if !device_present() {
        return;
    }

    let spawned = hil_swarm_test()
        .aot_cell(assert_ok!(build_aot_cell("event_subscribe")), SRI_SUB)
        .spawn()
        .await;

    let _monitor = assert_ok!(flash_device());

    // Act I - load cell (it subscribes as part of its init)
    let mut ctx = spawned.connect_deferred().await;
    let mut e_queue = ctx.subscribe_cell_event(EVENT_TWO).await;
    ctx.load_cells().await;

    // Act II - publish an event via sorg-client
    let expected = b"hello from sorg-client";
    ctx.publish_cell_event(EVENT_ONE, expected.to_vec()).await;

    // Assert - check that the cell received it to then published on the other event
    assert_matches!(e_queue.receive().await, Ok(ev) if ev == *expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn cell_to_cell_event_fan_out_emb_publisher() {
    if !device_present() {
        return;
    }

    // Publisher on the device, both subscribers on the host.
    let spawned = hil_swarm_test()
        .aot_cell(assert_ok!(build_aot_cell("event_publish")), SRI_PUB)
        .wasm_artifact_on(
            assert_ok!(build_wasm_cell("event_subscribe")),
            SRI_SUB,
            &[TAG_LINUX],
        )
        .wasm_artifact_on(
            assert_ok!(build_wasm_cell("event_subscribe_b")),
            SRI_SUB_B,
            &[TAG_LINUX],
        )
        .spawn()
        .await;

    let _monitor = assert_ok!(flash_device());

    let mut ctx = spawned.connect_deferred().await;
    let mut queue_a = ctx.subscribe_cell_event(EVENT_TWO).await;
    let mut queue_b = ctx.subscribe_cell_event(EVENT_THREE).await;
    ctx.load_cells().await;

    // Act - command the publisher to fire the event
    ctx.command_send(SRI_PUB, CMD_PUBLISH, None).await;

    // Assert - both subscribers should have forwarded the payload to their respective events
    let expected = b"pub_payload";

    assert_matches!(queue_a.receive().await, Ok(ev) if ev == *expected);
    assert_matches!(queue_b.receive().await, Ok(ev) if ev == *expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn event_to_embedded_delivers_sender() {
    if !device_present() {
        return;
    }

    // Publisher on the host, subscriber on the device: verifies the embedded subscriber sees the
    // event publisher's SRI in `md.sender`.
    let spawned = hil_swarm_test()
        .wasm_artifact_on(
            assert_ok!(build_wasm_cell("event_publish")),
            SRI_PUB,
            &[TAG_LINUX],
        )
        .aot_cell(assert_ok!(build_aot_cell("event_subscribe")), SRI_SUB)
        .spawn()
        .await;

    let _monitor = assert_ok!(flash_device());

    let mut ctx = spawned.connect_deferred().await;
    let mut q = ctx.subscribe_cell_event(SENDER_EVENT).await;
    ctx.load_cells().await;

    // Trigger the publisher to emit `my_event`; the embedded subscriber reports the event's
    // sender back on `evt_sender` as the raw 16-byte SRI.
    ctx.command_send(SRI_PUB, CMD_PUBLISH, None).await;

    let msg = assert_ok!(q.receive().await);
    let bytes: [u8; 16] = msg
        .as_slice()
        .try_into()
        .expect("evt_sender payload should be a 16-byte SRI");
    assert_eq!(
        Sri::from_bytes(bytes),
        Sri::from_target(SRI_PUB).expect("valid sri"),
        "embedded subscriber must see the event publisher's SRI in md.sender"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn cell_to_cell_event_fan_out_emb_sub() {
    if !device_present() {
        return;
    }

    // First subscriber on the device, publisher and second subscriber on the host.
    let spawned = hil_swarm_test()
        .wasm_artifact_on(
            assert_ok!(build_wasm_cell("event_publish")),
            SRI_PUB,
            &[TAG_LINUX],
        )
        .aot_cell(assert_ok!(build_aot_cell("event_subscribe")), SRI_SUB)
        .wasm_artifact_on(
            assert_ok!(build_wasm_cell("event_subscribe_b")),
            SRI_SUB_B,
            &[TAG_LINUX],
        )
        .spawn()
        .await;

    let _monitor = assert_ok!(flash_device());

    let mut ctx = spawned.connect_deferred().await;
    let mut queue_a = ctx.subscribe_cell_event(EVENT_TWO).await;
    let mut queue_b = ctx.subscribe_cell_event(EVENT_THREE).await;
    ctx.load_cells().await;

    // Act - command the publisher to fire the event
    ctx.command_send(SRI_PUB, CMD_PUBLISH, None).await;

    // Assert - both subscribers should have forwarded the payload to their respective events
    let expected = b"pub_payload";

    assert_matches!(queue_a.receive().await, Ok(ev) if ev == *expected);
    assert_matches!(queue_b.receive().await, Ok(ev) if ev == *expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn cell_to_cell_event_fan_out_emb_sub_b() {
    if !device_present() {
        return;
    }

    // Second subscriber on the device, publisher and first subscriber on the host.
    let spawned = hil_swarm_test()
        .wasm_artifact_on(
            assert_ok!(build_wasm_cell("event_publish")),
            SRI_PUB,
            &[TAG_LINUX],
        )
        .wasm_artifact_on(
            assert_ok!(build_wasm_cell("event_subscribe")),
            SRI_SUB,
            &[TAG_LINUX],
        )
        .aot_cell(assert_ok!(build_aot_cell("event_subscribe_b")), SRI_SUB_B)
        .spawn()
        .await;

    let _monitor = assert_ok!(flash_device());

    let mut ctx = spawned.connect_deferred().await;
    let mut queue_a = ctx.subscribe_cell_event(EVENT_TWO).await;
    let mut queue_b = ctx.subscribe_cell_event(EVENT_THREE).await;
    ctx.load_cells().await;

    // Act - command the publisher to fire the event
    ctx.command_send(SRI_PUB, CMD_PUBLISH, None).await;

    // Assert - both subscribers should have forwarded the payload to their respective events
    let expected = b"pub_payload";

    assert_matches!(queue_a.receive().await, Ok(ev) if ev == *expected);
    assert_matches!(queue_b.receive().await, Ok(ev) if ev == *expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn embedded_publish_stamps_sender() {
    if !device_present() {
        return;
    }

    // Publisher on the device, subscriber on the host: verifies the event published *by the
    // embedded cell* carries that cell's SRI in `md.sender`. The host subscriber republishes the
    // sender it observed back on `evt_sender` as the raw 16-byte SRI.
    let spawned = hil_swarm_test()
        .aot_cell(assert_ok!(build_aot_cell("event_publish")), SRI_PUB)
        .wasm_artifact_on(
            assert_ok!(build_wasm_cell("event_subscribe")),
            SRI_SUB,
            &[TAG_LINUX],
        )
        .spawn()
        .await;

    let _monitor = assert_ok!(flash_device());

    let mut ctx = spawned.connect_deferred().await;
    let mut q = ctx.subscribe_cell_event(SENDER_EVENT).await;
    ctx.load_cells().await;

    // Trigger the embedded publisher to emit `my_event`; the host subscriber reports the event's
    // sender back on `evt_sender` as the raw 16-byte SRI.
    ctx.command_send(SRI_PUB, CMD_PUBLISH, None).await;

    let msg = assert_ok!(q.receive().await);
    let bytes: [u8; 16] = msg
        .as_slice()
        .try_into()
        .expect("evt_sender payload should be a 16-byte SRI");
    assert_eq!(
        Sri::from_bytes(bytes),
        Sri::from_target(SRI_PUB).expect("valid sri"),
        "event published by the embedded cell must carry its own SRI in md.sender"
    );
}
