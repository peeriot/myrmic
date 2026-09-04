//! Event pub/sub integration tests.
//!
//! Migrated to the new cell model: commands are fire-and-forget (`command_send`),
//! event subscription is runtime-driven (the runtime auto-subscribes to a cell's
//! discovered `event_*` handlers), and the host observes results via
//! `subscribe_cell_event`.

use std::time::Duration;

use claims::{assert_none, assert_ok};
use sorg_tests::{build_cell, swarm_config};

use crate::integration::spawn_test_app_with_swarm;

const EVENT_ONE: &str = "my_event";
const EVENT_TWO: &str = "other_event";
const EVENT_THREE: &str = "third_event";

const CMD_PUBLISH: &str = "publish_event";

const SRI_PUB: &str = "publisher";
const SRI_SUB: &str = "subscriber";
const SRI_SUB_B: &str = "subscriber_b";

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn sorg_client_event_pub_sub() {
    // Arrange – bring up a minimal swarm so we have a running zenoh + plugins
    let swarm = swarm_config!("cells/events/swarm.jsonnet");
    let mut test_app = spawn_test_app_with_swarm(swarm).await;

    // Act I – subscribe to the first event
    let mut event_queue = test_app.subscribe_cell_event(EVENT_ONE).await;

    // Act II – publish on the subscribed event
    let payload = b"hello world".to_vec();
    test_app
        .publish_cell_event(EVENT_ONE, payload.clone())
        .await;

    // Assert I - we have subscribed so we expect to receive it
    let received = assert_ok!(event_queue.receive().await);
    assert_eq!(payload, received);

    // Act III – publish on a different event
    let other_payload = b"should not be received".to_vec();
    test_app.publish_cell_event(EVENT_TWO, other_payload).await;

    // Assert II – we should not receive this one at the event queue
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_none!(assert_ok!(event_queue.try_receive().await));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn cell_publishes_event() {
    // Arrange - bring up a swarm and build the test publisher
    let swarm = swarm_config!("cells/events/swarm.jsonnet");
    build_cell("../../tests/fixtures/event_publish", &swarm).await;

    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut e_queue = test_app.subscribe_cell_event(EVENT_ONE).await;

    // Act - load cell + command it to publish (fire-and-forget)
    test_app
        .deploy_wasm_cell("event_publish.wasm".to_owned(), SRI_PUB.to_owned())
        .await;
    test_app.command_send(SRI_PUB, CMD_PUBLISH, None).await;

    // Assert - we expect to receive the event
    let expected = b"pub_payload";
    let received = assert_ok!(e_queue.receive().await);
    assert_eq!(*expected, *received);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn cell_subscribes_to_event() {
    // Arrange - bring up swarm and build the subscriber cell
    let swarm = swarm_config!("cells/events/swarm.jsonnet");
    build_cell("../../tests/fixtures/event_subscribe", &swarm).await;

    let mut test_app = spawn_test_app_with_swarm(swarm).await;

    let mut e_queue = test_app.subscribe_cell_event(EVENT_TWO).await; // event the sub cell pubs to in its handler

    // Act I - load cell (runtime auto-subscribes it to `my_event` from its `event_my_event` export)
    test_app
        .deploy_wasm_cell("event_subscribe.wasm".to_owned(), SRI_SUB.to_owned())
        .await;

    // Act II - publish an event via sorg-client
    let expected = b"hello from sorg-client";
    let payload = expected.to_vec();
    test_app
        .publish_cell_event(EVENT_ONE, payload.clone())
        .await;

    // Assert - check that the cell received it then published on the other event
    let received = assert_ok!(e_queue.receive().await);
    assert_eq!(*expected, *received);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn cell_to_cell_event_fan_out() {
    // Arrange - build all three modules: publisher + two subscribers forwarding to different events
    let swarm = swarm_config!("cells/events/swarm.jsonnet");
    build_cell("../../tests/fixtures/event_publish", &swarm).await;
    build_cell("../../tests/fixtures/event_subscribe", &swarm).await;
    build_cell("../../tests/fixtures/event_subscribe_b", &swarm).await;
    let mut test_app = spawn_test_app_with_swarm(swarm).await;

    // Subscribe the sorg-client to both forwarded events
    let mut queue_a = test_app.subscribe_cell_event(EVENT_TWO).await;
    let mut queue_b = test_app.subscribe_cell_event(EVENT_THREE).await;

    // Load publisher and both subscriber cells
    test_app
        .deploy_wasm_cell("event_publish.wasm".to_owned(), SRI_PUB.to_owned())
        .await;
    test_app
        .deploy_wasm_cell("event_subscribe.wasm".to_owned(), SRI_SUB.to_owned())
        .await;
    test_app
        .deploy_wasm_cell("event_subscribe_b.wasm".to_owned(), SRI_SUB_B.to_owned())
        .await;

    // Act - command the publisher to fire the event (fire-and-forget)
    test_app.command_send(SRI_PUB, CMD_PUBLISH, None).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Assert - both subscribers should have forwarded the payload to their respective events
    let expected = b"pub_payload";

    let received_a = assert_ok!(queue_a.receive().await);
    assert_eq!(*expected, *received_a);

    let received_b = assert_ok!(queue_b.receive().await);
    assert_eq!(*expected, *received_b);
}
