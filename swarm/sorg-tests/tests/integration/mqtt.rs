use rumqttc::QoS;
use sorg_tests::{TestApp, enable_test_logging, swarm_config};

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn can_sub_and_pub() {
    enable_test_logging("debug");
    let empty_swarm = swarm_config!("empty.jsonnet");
    // Arrange - set up a test app; set up the test mqtt broker; subscribe to two topics
    let mut test_app: TestApp = TestApp::spawn(empty_swarm, || async { true }).await; // no swarm, health check always ok
    let (_broker_handle, mqtt_client) = test_app.set_up_mqtt_broker().await;
    mqtt_client
        .subscribe(topic_one(), QoS::AtMostOnce)
        .await
        .expect("failed to sub topic 1");
    mqtt_client
        .subscribe(topic_two(), QoS::AtMostOnce)
        .await
        .expect("failet to sub topic 2");
    let payload = 42_u16;

    // Act - Send a message to first topic
    let bytes = payload.to_be_bytes().to_vec();
    test_app.send_mqtt_msg(&topic_one(), bytes).await;

    // Assert - we expect to get the message on first topic; not get a message on second; not get a message on third where we didn't subscribe
    let mut received_1 = test_app.received_msgs_on_mqtt_topic(&topic_one()).await;
    assert_eq!(1, received_1.len());
    let msg = received_1.swap_remove(0);
    let bytes: [u8; 2] = msg.try_into().unwrap();
    let num = u16::from_be_bytes(bytes);
    assert_eq!(payload, num);

    let received_2 = test_app.received_msgs_on_mqtt_topic(&topic_two()).await;
    assert!(received_2.is_empty());

    let received_3 = test_app.received_msgs_on_mqtt_topic(&topic_three()).await;
    assert!(received_3.is_empty());
}

fn topic_one() -> String {
    "topic_one".to_owned()
}
fn topic_two() -> String {
    "topic_two".to_owned()
}
fn topic_three() -> String {
    "topic_three".to_owned()
}
