//! Embedded MQTT broker and subscription helpers for e2e tests.

use std::{collections::HashMap, sync::Arc, time::Duration};

use config::{File, FileFormat};
use rumqttc::{AsyncClient, Event, EventLoop, MqttOptions, Packet, QoS};
use rumqttd::{Broker, Config};
use tokio::sync::{Mutex, mpsc};

/// Raw MQTT message payload.
pub type Payload = Vec<u8>;
type SubscriptionSender = mpsc::Sender<Payload>;
type Subscriptions = Arc<Mutex<HashMap<String, Vec<SubscriptionSender>>>>;

/// Embedded MQTT broker backed by `rumqttd`.
///
/// Drop to stop accepting connections (the broker thread keeps running until
/// the process exits, but existing connections will be cleaned up).
pub struct MqttBroker {
    _broker_thread: std::thread::JoinHandle<()>,
    subscriptions: Subscriptions,
    client: AsyncClient,
}

impl MqttBroker {
    /// Start a broker listening on `port` and spawn the internal monitor client.
    pub async fn start(port: u16) -> Self {
        let config_toml = broker_config_toml(port);
        let config: Config = config::Config::builder()
            .add_source(File::from_str(&config_toml, FileFormat::Toml))
            .build()
            .expect("failed to build mqtt broker config")
            .try_deserialize()
            .expect("failed to deserialize mqtt broker config");

        let broker_thread = std::thread::spawn(move || {
            let mut broker = Broker::new(config);
            broker.start().expect("mqtt broker failed");
        });

        // Give the broker a moment to bind.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let subscriptions: Subscriptions = Arc::default();

        let (client, event_loop) = connect("mqtt_broker_monitor", port);
        Self::spawn_event_loop(event_loop, Arc::clone(&subscriptions));

        // Wait for the monitor connection to be acknowledged.
        tokio::time::sleep(Duration::from_millis(100)).await;

        Self {
            _broker_thread: broker_thread,
            subscriptions,
            client,
        }
    }

    /// Subscribe to `topic` and return a [`MqttSubscription`] that receives
    /// each published payload.
    pub async fn subscribe(&self, topic: impl Into<String>) -> MqttSubscription {
        let topic = topic.into();
        let (tx, rx) = mpsc::channel(64);

        self.subscriptions
            .lock()
            .await
            .entry(topic.clone())
            .or_default()
            .push(tx);

        self.client
            .subscribe(&topic, QoS::AtMostOnce)
            .await
            .expect("mqtt subscribe failed");

        // Wait for the SUBACK to be processed.
        tokio::time::sleep(Duration::from_millis(100)).await;

        MqttSubscription { receiver: rx }
    }

    /// Publish a message to `topic`.
    pub async fn publish(&self, topic: impl Into<String>, payload: Vec<u8>) {
        self.client
            .publish(topic.into(), QoS::AtMostOnce, false, payload)
            .await
            .expect("mqtt publish failed");
    }

    /// Publish `payload` to `topic` every 500ms until `subscription` receives a
    /// message, then return that message. Panics if nothing arrives within `timeout`.
    ///
    /// Use this instead of sleeping for "the bridge has surely subscribed by now":
    /// it makes the first successful round-trip the observable condition.
    pub async fn publish_until_received(
        &self,
        topic: impl Into<String>,
        payload: Vec<u8>,
        subscription: &mut MqttSubscription,
        timeout: Duration,
    ) -> Payload {
        let topic = topic.into();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            self.publish(topic.clone(), payload.clone()).await;
            if let Some(message) = subscription.recv_timeout(Duration::from_millis(500)).await {
                return message;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "no message received on subscription within {timeout:?} while publishing to `{topic}`"
            );
        }
    }

    fn spawn_event_loop(mut event_loop: EventLoop, subscriptions: Subscriptions) {
        tokio::spawn(async move {
            loop {
                match event_loop.poll().await {
                    Ok(Event::Incoming(Packet::Publish(publish))) => {
                        let mut subs = subscriptions.lock().await;
                        if let Some(senders) = subs.get_mut(&publish.topic) {
                            let payload = publish.payload.to_vec();
                            senders.retain(|tx| tx.try_send(payload.clone()).is_ok());
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });
    }
}

/// Receiver side of an MQTT topic subscription.
pub struct MqttSubscription {
    receiver: mpsc::Receiver<Payload>,
}

impl MqttSubscription {
    /// Wait for the next message, up to `timeout`.
    pub async fn recv_timeout(&mut self, timeout: Duration) -> Option<Payload> {
        tokio::time::timeout(timeout, self.receiver.recv())
            .await
            .ok()
            .flatten()
    }

    /// Collect exactly `count` messages, waiting up to `timeout` total.
    pub async fn recv_n(
        &mut self,
        count: usize,
        timeout: Duration,
    ) -> Result<Vec<Payload>, String> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut messages = Vec::with_capacity(count);
        while messages.len() < count {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(remaining, self.receiver.recv()).await {
                Ok(Some(msg)) => messages.push(msg),
                Ok(None) => return Err("subscription channel closed".to_owned()),
                Err(_) => {
                    return Err(format!(
                        "timed out waiting for {count} messages, got {}",
                        messages.len()
                    ));
                }
            }
        }
        Ok(messages)
    }
}

fn connect(client_id: &str, port: u16) -> (AsyncClient, EventLoop) {
    let opts = MqttOptions::new(client_id, "localhost", port);
    AsyncClient::new(opts, 10)
}

fn broker_config_toml(port: u16) -> String {
    format!(
        r#"
id = 0

[router]
id = 0
max_connections = 100
max_outgoing_packet_count = 200
max_segment_size = 104857600
max_segment_count = 10

[v4.1]
name = "v4-1"
listen = "0.0.0.0:{port}"
next_connection_delay_ms = 1
[v4.1.connections]
connection_timeout_ms = 60000
max_payload_size = 20480
max_inflight_count = 100
dynamic_filters = true
"#
    )
}
