use std::{
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use config::{File, FileFormat};
use rumqttc::{AsyncClient, Event, EventLoop, MqttOptions, Outgoing, Packet};
use rumqttd::{Broker, Config};
use tokio::{
    net::{TcpListener, TcpStream},
    task::AbortHandle,
};

use crate::{TestApp, WAIT_TIME};

const BROKER_HOST: &str = "localhost";
const BROKER_PORT: u16 = 1883;
const PROXY_HOST: &str = "127.0.0.1";

/// How long the broker gets to accept its first connection.
const BROKER_START_TIMEOUT: Duration = Duration::from_secs(10);

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
        wait_for_broker().await;

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

/// A TCP relay in front of the test broker whose link can be cut and restored: what
/// one client sees of a broker restart or a network outage.
pub struct MqttOutageProxy {
    port: u16,
    up: Arc<AtomicBool>,
    relays: Arc<Mutex<Vec<AbortHandle>>>,
    listener: tokio::task::JoinHandle<()>,
}

impl MqttOutageProxy {
    /// Binds a free local port and relays every connection on it to the test broker.
    pub async fn start() -> Self {
        let listener = TcpListener::bind((PROXY_HOST, 0))
            .await
            .expect("failed to bind the mqtt outage proxy");
        let port = listener
            .local_addr()
            .expect("the mqtt outage proxy has no local address")
            .port();
        let up = Arc::new(AtomicBool::new(true));
        let relays: Arc<Mutex<Vec<AbortHandle>>> = Arc::default();

        let listener = tokio::spawn({
            let up = Arc::clone(&up);
            let relays = Arc::clone(&relays);
            async move {
                loop {
                    let Ok((mut client, _)) = listener.accept().await else {
                        continue;
                    };
                    // While cut, hang up on every connection attempt.
                    if !up.load(Ordering::SeqCst) {
                        drop(client);
                        continue;
                    }
                    let Ok(mut broker) = TcpStream::connect((BROKER_HOST, BROKER_PORT)).await
                    else {
                        continue;
                    };
                    let relay = tokio::spawn(async move {
                        let _ = tokio::io::copy_bidirectional(&mut client, &mut broker).await;
                    });
                    // `cut` flips `up` before draining the list under this lock, so a
                    // relay that landed in between is caught here instead of surviving.
                    let mut relays = relays.lock().unwrap_or_else(PoisonError::into_inner);
                    if up.load(Ordering::SeqCst) {
                        relays.push(relay.abort_handle());
                    } else {
                        relay.abort();
                    }
                }
            }
        });

        Self {
            port,
            up,
            relays,
            listener,
        }
    }

    /// The broker URL a bridge is deployed with to reach the broker through this proxy.
    pub fn broker_url(&self) -> String {
        format!("mqtt://{PROXY_HOST}:{}", self.port)
    }

    /// Drops every live connection and hangs up on new ones until [`Self::restore`].
    pub fn cut(&self) {
        self.up.store(false, Ordering::SeqCst);
        for relay in self
            .relays
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain(..)
        {
            relay.abort();
        }
    }

    /// Lets connections through to the broker again.
    pub fn restore(&self) {
        self.up.store(true, Ordering::SeqCst);
    }
}

impl Drop for MqttOutageProxy {
    fn drop(&mut self) {
        self.cut();
        self.listener.abort();
    }
}

fn connect_to_test_broker(client_id: &str) -> (AsyncClient, EventLoop) {
    let mqtt_options = MqttOptions::new(client_id, BROKER_HOST, BROKER_PORT);
    AsyncClient::new(mqtt_options, 10)
}

/// The broker binds on a thread of its own, so there is no handle to await and
/// no readiness signal to subscribe to. Probing the port is the signal: until it
/// accepts, a client's first poll fails outright with `ConnectionRefused`, and
/// the subscribe that follows fails with it.
async fn wait_for_broker() {
    let deadline = Instant::now() + BROKER_START_TIMEOUT;
    loop {
        if TcpStream::connect((BROKER_HOST, BROKER_PORT)).await.is_ok() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the test broker did not accept a connection within \
             {BROKER_START_TIMEOUT:?}"
        );
        tokio::time::sleep(WAIT_TIME).await;
    }
}

async fn poll_until_publish_goes_out(event_loop: &mut EventLoop) {
    loop {
        let event = event_loop.poll().await.expect("failed to poll event loop");
        if let Event::Outgoing(Outgoing::Publish(..)) = event {
            break;
        }
    }
}
