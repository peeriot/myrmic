use std::sync::Arc;

use rumqttc::{AsyncClient, EventLoop, MqttOptions, TlsConfiguration, Transport};
use rustls::{ClientConfig, RootCertStore, crypto::ring::default_provider};
use sorg_common::custom_err;

use crate::Result;

pub use sorg_common::MqttConnection;

/// Connects to the broker; an `mqtts` connection trusts the webpki roots only.
pub(crate) async fn connect_to_broker(
    mqtt_connection: &MqttConnection,
) -> Result<(AsyncClient, EventLoop)> {
    let roots = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };

    connect_with_roots(mqtt_connection, roots).await
}

/// Like [`connect_to_broker`], but trusting `roots`. Production always goes through
/// `connect_to_broker`; this only exists so tests can trust a test CA.
async fn connect_with_roots(
    mqtt_connection: &MqttConnection,
    roots: RootCertStore,
) -> Result<(AsyncClient, EventLoop)> {
    let MqttConnection {
        broker_address,
        broker_port,
        channel_cap,
        client_id,
        keep_alive_period,
        tls,
    } = mqtt_connection;
    let mut mqtt_options = MqttOptions::new(client_id, broker_address.as_ref(), *broker_port);
    mqtt_options.set_keep_alive(*keep_alive_period);
    if *tls {
        mqtt_options.set_transport(Transport::tls_with_config(tls_config(roots)?));
    }

    let (client, mut event_loop) = AsyncClient::new(mqtt_options, *channel_cap as usize);
    // poll the event loop to make sure that connection is there
    event_loop.poll().await.map_err(|err| {
        custom_err!(
            "failed to connect to mqtt broker on '{address}':'{port}' - {err}",
            address = broker_address.as_ref(),
            port = broker_port,
        )
    })?;
    Ok((client, event_loop))
}

/// The repo installs no process-wide crypto provider and the workspace enables more
/// than one, so the provider is passed explicitly instead of `ClientConfig::builder()`.
fn tls_config(roots: RootCertStore) -> Result<TlsConfiguration> {
    let provider = Arc::new(default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|err| custom_err!("failed to build the mqtt tls config - {err}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();

    Ok(TlsConfiguration::Rustls(Arc::new(config)))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sorg_common::MqttBrokerAddress;
    use sorg_tests::TlsTestBroker;
    use tokio::time::timeout;

    use super::{MqttConnection, connect_to_broker, connect_with_roots};

    const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

    #[tokio::test]
    async fn mqtts_connects_to_a_tls_broker_and_verifies_it() {
        let broker = TlsTestBroker::start().await;

        let trusted = timeout(
            CONNECT_TIMEOUT,
            connect_with_roots(
                &connection("localhost", broker.tls_port, true),
                broker.root_store(),
            ),
        )
        .await
        .expect("tls connect timed out");
        assert!(
            trusted.is_ok(),
            "tls connect with the test CA failed: {:?}",
            trusted.err()
        );

        let wrong_name = timeout(
            CONNECT_TIMEOUT,
            connect_with_roots(
                &connection("127.0.0.1", broker.tls_port, true),
                broker.root_store(),
            ),
        )
        .await
        .expect("tls connect timed out")
        .err()
        .expect("a cert for localhost must not be accepted for 127.0.0.1")
        .to_string();
        assert!(
            wrong_name.contains(r#"certificate not valid for name "127.0.0.1""#),
            "unexpected error: {wrong_name}"
        );

        let production_roots = timeout(
            CONNECT_TIMEOUT,
            connect_to_broker(&connection("localhost", broker.tls_port, true)),
        )
        .await
        .expect("tls connect timed out")
        .err()
        .expect("the webpki roots must not trust the test CA")
        .to_string();
        assert!(
            production_roots.contains("UnknownIssuer"),
            "unexpected error: {production_roots}"
        );
    }

    #[tokio::test]
    async fn mqtts_against_a_plaintext_listener_fails_without_fallback() {
        let broker = TlsTestBroker::start().await;

        // Positive control: the listener does speak plaintext MQTT.
        let plain = timeout(
            CONNECT_TIMEOUT,
            connect_with_roots(
                &connection("localhost", broker.plain_port, false),
                broker.root_store(),
            ),
        )
        .await
        .expect("plain connect timed out");
        assert!(plain.is_ok(), "plaintext connect failed: {:?}", plain.err());

        let err = timeout(
            CONNECT_TIMEOUT,
            connect_with_roots(
                &connection("localhost", broker.plain_port, true),
                broker.root_store(),
            ),
        )
        .await
        .expect("tls connect timed out")
        .err()
        .expect("mqtts against a plaintext listener must not connect")
        .to_string();
        assert!(err.contains("TLS:"), "expected a TLS error, got: {err}");
    }

    #[tokio::test]
    async fn mqtts_connects_to_an_ipv6_literal_with_an_ip_san() {
        let broker = TlsTestBroker::start().await;
        let Some(port) = broker.tls_v6_port else {
            eprintln!("skipping: this host has no IPv6 loopback");
            return;
        };

        let connected = timeout(
            CONNECT_TIMEOUT,
            connect_with_roots(&connection("::1", port, true), broker.root_store()),
        )
        .await
        .expect("tls connect timed out");
        assert!(
            connected.is_ok(),
            "tls connect to ::1 failed: {:?}",
            connected.err()
        );
    }

    fn connection(host: &str, port: u16, tls: bool) -> MqttConnection {
        MqttConnection {
            broker_address: MqttBrokerAddress::new(host).expect("valid broker address"),
            broker_port: port,
            keep_alive_period: Duration::from_secs(10),
            client_id: format!("tls-test-{port}-{tls}"),
            channel_cap: 16,
            tls,
        }
    }
}
