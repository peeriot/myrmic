use std::{thread::JoinHandle, time::Duration};

use config::{File, FileFormat};
use rumqttc::{AsyncClient, Event, EventLoop, MqttOptions, Outgoing, Packet};
use rumqttd::{Broker, Config};

use crate::{TestApp, WAIT_TIME};

impl TestApp {
    /// Sets up an MQTT broker which acts like an "external" broker in tests, in the sense
    /// of not being provided via a swarm plugin
    pub async fn set_up_mqtt_broker(&mut self) -> (JoinHandle<()>, AsyncClient) {
        let config_str = include_str!("../mqtt_broker_config.toml");
        let config: Config = config::Config::builder()
            .add_source(File::from_str(config_str, FileFormat::Toml))
            .build()
            .expect("failed to build mqtt config")
            .try_deserialize()
            .expect("failed to deserialize mqtt config");

        // using a thread here instead of 'spawn_blocking', since the tests are run with a single-threaded
        // runtime which would block
        let broker_handle = std::thread::spawn(move || {
            let mut broker = Broker::new(config);
            broker.start().expect("failed to start broker");
        });
        tokio::time::sleep(WAIT_TIME).await;

        // start up a tokio task to receive messages we may subscribe to during tests
        let received_msgs = self.received_mqtt_msgs.clone();
        let (client, mut event_loop) = connect_to_test_broker("test_client_handle");
        tokio::spawn(async move {
            loop {
                if let Event::Incoming(Packet::Publish(publish)) =
                    event_loop.poll().await.expect("error polling event loop")
                {
                    received_msgs
                        .lock()
                        .await
                        .entry(publish.topic)
                        .or_default()
                        .push(publish.payload.to_vec());
                }
            }
        });
        tokio::time::sleep(WAIT_TIME).await;
        (broker_handle, client)
    }

    pub async fn subscribe_to_mqtt_topic(&self, client: &AsyncClient, topic: &str) {
        client
            .subscribe(topic, rumqttc::QoS::AtMostOnce)
            .await
            .expect("subscribe failed");
        tokio::time::sleep(WAIT_TIME).await;
    }

    pub async fn send_mqtt_msg(&self, topic: &str, payload: Vec<u8>) {
        let (client, mut event_loop) = connect_to_test_broker("test_client_send");
        client
            .publish(topic, rumqttc::QoS::AtMostOnce, false, payload)
            .await
            .expect("failed publishing");
        poll_until_publish_goes_out(&mut event_loop).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    /// Returns the messages received by the testapp on the provided mqtt topic (it should have subscribed to it earlier)
    /// this method drains the message buffer, so that a subsequent call will not find the same messages there
    pub async fn received_msgs_on_mqtt_topic(&mut self, topic: &str) -> Vec<Vec<u8>> {
        let mut guard = self.received_mqtt_msgs.lock().await;
        let Some(msgs) = guard.get_mut(topic) else {
            return vec![];
        };
        std::mem::take(msgs)
    }
}

fn connect_to_test_broker(client_id: &str) -> (AsyncClient, EventLoop) {
    let mqtt_options = MqttOptions::new(client_id, "localhost", 1883);
    AsyncClient::new(mqtt_options, 10)
}

async fn poll_until_publish_goes_out(event_loop: &mut EventLoop) {
    loop {
        let event = event_loop.poll().await.expect("failed to poll event loop");
        if let Event::Outgoing(Outgoing::Publish(..)) = event {
            break;
        }
    }
}
