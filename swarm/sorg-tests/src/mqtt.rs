use std::{
    net::TcpListener as StdTcpListener,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use config::{File, FileFormat};
use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use rumqttc::{AsyncClient, Event, EventLoop, MqttOptions, Outgoing, Packet};
use rumqttd::{Broker, Config};
use rustls::{RootCertStore, crypto::ring::default_provider, pki_types::CertificateDer};
use tempfile::{TempDir, tempdir};
use tokio::{
    fs,
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
        let broker_handle = thread::spawn(move || {
            let mut broker = Broker::new(config);
            broker.start().expect("failed to start broker");
        });
        wait_for_broker(BROKER_HOST, BROKER_PORT).await;

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

/// A broker with one plaintext and one TLS (test CA, cert for `localhost` only)
/// v4 listener on free ports, for testing the bridge's `mqtts://` transport. The broker
/// thread lives until the process ends, there is no shutdown on drop.
pub struct TlsTestBroker {
    pub plain_port: u16,
    pub tls_port: u16,
    /// Port of the TLS listener on `[::1]`, `None` when the host has no `IPv6` loopback.
    pub tls_v6_port: Option<u16>,
    ca_der: CertificateDer<'static>,
    // keeps the generated PEM files alive for as long as the broker is
    _dir: TempDir,
}

impl TlsTestBroker {
    pub async fn start() -> Self {
        // rumqttd needs a process default provider and the workspace enables ring and
        // aws-lc-rs, so pick ring. Err means another test already installed one.
        default_provider().install_default().unwrap_or_default();

        let [plain_port, tls_port] = free_ports();
        let tls_v6_port = StdTcpListener::bind("[::1]:0")
            .ok()
            .and_then(|listener| listener.local_addr().ok())
            .map(|addr| addr.port());
        let (ca_der, cert_pem, key_pem) = generate_test_pki();
        let tmp = tempdir().expect("failed to create the tls tempdir");
        let dir = tmp.path().display();
        fs::write(tmp.path().join("server.cert.pem"), cert_pem)
            .await
            .expect("failed to write the server cert");
        fs::write(tmp.path().join("server.key.pem"), key_pem)
            .await
            .expect("failed to write the server key");
        let v6_listener = tls_v6_port
            .map(|port| {
                format!(
                    r#"
[v4.3]
name = "tls6"
listen = "[::1]:{port}"
next_connection_delay_ms = 1
[v4.3.tls]
certpath = "{dir}/server.cert.pem"
keypath = "{dir}/server.key.pem"
[v4.3.connections]
connection_timeout_ms = 5000
max_payload_size = 20480
max_inflight_count = 100
dynamic_filters = true
"#
                )
            })
            .unwrap_or_default();
        let config_str = format!(
            r#"
id = 0
[router]
id = 0
max_connections = 100
max_outgoing_packet_count = 200
max_segment_size = 104857600
max_segment_count = 10

[v4.1]
name = "plain"
listen = "127.0.0.1:{plain_port}"
next_connection_delay_ms = 1
[v4.1.connections]
connection_timeout_ms = 5000
max_payload_size = 20480
max_inflight_count = 100
dynamic_filters = true

[v4.2]
name = "tls"
listen = "127.0.0.1:{tls_port}"
next_connection_delay_ms = 1
[v4.2.tls]
certpath = "{dir}/server.cert.pem"
keypath = "{dir}/server.key.pem"
[v4.2.connections]
connection_timeout_ms = 5000
max_payload_size = 20480
max_inflight_count = 100
dynamic_filters = true
{v6_listener}"#
        );
        let config: Config = config::Config::builder()
            .add_source(File::from_str(&config_str, FileFormat::Toml))
            .build()
            .expect("failed to build mqtt tls config")
            .try_deserialize()
            .expect("failed to deserialize mqtt tls config");

        thread::spawn(move || {
            Broker::new(config)
                .start()
                .expect("failed to start tls broker");
        });
        wait_for_broker(BROKER_HOST, plain_port).await;
        wait_for_broker(BROKER_HOST, tls_port).await;
        if let Some(port) = tls_v6_port {
            wait_for_broker("::1", port).await;
        }

        Self {
            plain_port,
            tls_port,
            tls_v6_port,
            ca_der,
            _dir: tmp,
        }
    }

    /// A root store trusting only this broker's generated test CA.
    pub fn root_store(&self) -> RootCertStore {
        let mut roots = RootCertStore::empty();
        roots
            .add(self.ca_der.clone())
            .expect("failed to add the test CA");
        roots
    }
}

/// A fresh CA plus a server cert for `localhost` and `::1` (not `127.0.0.1`, so that
/// fails host name verification). Returns the CA cert (DER), the server cert and key (PEM).
fn generate_test_pki() -> (CertificateDer<'static>, String, String) {
    let ca_key = KeyPair::generate().expect("failed to generate the CA key");
    let mut ca_params = CertificateParams::new(Vec::new()).expect("invalid CA params");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let ca = ca_params
        .self_signed(&ca_key)
        .expect("failed to self-sign the CA");

    let server_key = KeyPair::generate().expect("failed to generate the server key");
    let server = CertificateParams::new(vec!["localhost".to_string(), "::1".to_string()])
        .expect("invalid server params")
        .signed_by(&server_key, &Issuer::new(ca_params, ca_key))
        .expect("failed to sign the server cert");

    (ca.der().clone(), server.pem(), server_key.serialize_pem())
}

/// Both ports are bound at the same time, so they cannot collide.
fn free_ports() -> [u16; 2] {
    let listeners =
        [(); 2].map(|()| StdTcpListener::bind("127.0.0.1:0").expect("failed to bind a free port"));

    listeners.map(|listener| {
        listener
            .local_addr()
            .expect("failed to read the free port")
            .port()
    })
}

fn connect_to_test_broker(client_id: &str) -> (AsyncClient, EventLoop) {
    let mqtt_options = MqttOptions::new(client_id, BROKER_HOST, BROKER_PORT);
    AsyncClient::new(mqtt_options, 10)
}

/// The broker binds on a thread of its own, so there is no handle to await and
/// no readiness signal to subscribe to. Probing the port is the signal: until it
/// accepts, a client's first poll fails outright with `ConnectionRefused`, and
/// the subscribe that follows fails with it.
async fn wait_for_broker(host: &str, port: u16) {
    let deadline = Instant::now() + BROKER_START_TIMEOUT;
    loop {
        if TcpStream::connect((host, port)).await.is_ok() {
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
