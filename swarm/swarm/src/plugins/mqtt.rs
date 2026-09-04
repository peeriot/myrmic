use futures_util::future::select;
use rumqttd::Notification;
use rumqttd::local::{LinkRx, LinkTx};
use rumqttd::protocol::{Filter, Packet, Publish, QoS, RetainForwardRule, Subscribe};
use std::net::{Ipv4Addr, SocketAddr};
use std::pin::pin;
use std::time::Duration;
use swarm_api::DropNotifier;
use tokio::runtime::Handle;

use crate::plugins::MyrmicCtx;
use config::Config;
use zenoh::sample::Locality;
use zenoh::{Result as ZResult, Session};

mod config;

pub struct Plugin;

impl crate::plugins::MyrmicPlugin for Plugin {
    const DEFAULT_NAME: &'static str = "mqtt";

    type Config = Config;

    async fn main(ctx: MyrmicCtx, config: Self::Config) -> ZResult<()> {
        let session = ctx.session().clone();
        let handle = ctx.handle().clone();
        let drop_rx = ctx.drop_notifier();

        let Config {
            router,
            mut v4,
            mut v5,
            ws,
            allow,
        } = config;

        // it has bridge link support, but it's not exposed yet.
        // I need to create a PR in rumqttd to expose it.

        let router = router.unwrap_or_else(default_router);
        let router = rumqttd::Router::new(0, router);
        let router_tx = router.spawn();
        if v4.is_empty() {
            v4.push(default_v4());
        }
        if v5.is_empty() {
            v5.push(default_v5());
        }
        for settings in v4 {
            handle.spawn(spawn_server(
                settings,
                router_tx.clone(),
                rumqttd::LinkType::Remote,
                rumqttd::protocol::v4::V4,
                drop_rx.clone(),
            ));
        }
        for settings in v5 {
            handle.spawn(spawn_server(
                settings,
                router_tx.clone(),
                rumqttd::LinkType::Remote,
                rumqttd::protocol::v5::V5,
                drop_rx.clone(),
            ));
        }
        for settings in ws {
            handle.spawn(spawn_server(
                settings,
                router_tx.clone(),
                rumqttd::LinkType::Websocket,
                rumqttd::protocol::v4::V4,
                drop_rx.clone(),
            ));
        }

        let client_id = format!("zenoh-bridge-{}", session.zid());
        let (mut link_tx, link_rx, _ack) =
            rumqttd::local::LinkBuilder::new(&client_id, router_tx.clone())
                .dynamic_filters(true)
                .build()
                .expect("Unable to setup router link");
        handle.spawn(forward(session.clone(), link_rx));

        let filters = subscribe_topics(&session, &allow, &handle, &drop_rx, &mut link_tx).await;
        let subscribe = Subscribe { pkid: 0, filters };
        tracing::info!("sending subscription to mqtt broker");
        link_tx
            .send(Packet::Subscribe(subscribe, None))
            .await
            .expect("unable to subscribe");

        ctx.notify_ready();

        Ok(())
    }
}

async fn subscribe_topics(
    session: &Session,
    allow: &[config::Topic],
    handle: &Handle,
    drop_rx: &DropNotifier,
    link_tx: &mut LinkTx,
) -> Vec<Filter> {
    let mut filters = Vec::with_capacity(allow.len());

    for topic in allow {
        {
            let mqtt_topic = topic.as_mqtt();
            tracing::info!(topic = %mqtt_topic, "subscribe mqtt topic");

            filters.push(Filter {
                path: mqtt_topic.into(),
                qos: QoS::AtMostOnce,
                nolocal: true,
                preserve_retain: false,
                retain_forward_rule: RetainForwardRule::Never,
            });
        }

        {
            let zenoh_topic = topic.as_zenoh();
            tracing::info!(topic = %zenoh_topic, "subscribe zenoh topic");

            let sub = session
                .declare_subscriber(&*zenoh_topic)
                .allowed_origin(Locality::Remote)
                .callback({
                    let link_tx = link_tx.clone();

                    move |mut t| {
                        let mut link_tx = link_tx.clone();

                        let expr = t.key_expr();
                        tracing::debug!(
                            expr = %expr,
                            timestamp = ?t.timestamp(),
                            "receiving zenoh publish"
                        );

                        if expr.is_wild() {
                            tracing::info!(
                            "Zenoh topic contained wildcards, which aren't supported on mqtt: {}",
                            expr
                        );
                            return;
                        }

                        tracing::debug!(expr = %expr, "forwarding zenoh publish to mqtt");

                        let topic = String::from(expr.as_str()).into_bytes();
                        let payload = std::mem::take(t.payload_mut());
                        let payload = payload.to_bytes().to_vec();

                        let publish = Publish::new(topic, payload, false);

                        if let Err(err) = link_tx.push(Packet::Publish(publish, None)) {
                            tracing::error!("Unable to bridge zenoh-mqtt: {}", err);
                        }
                    }
                })
                .await
                .expect("unable to subscribe");

            handle.spawn({
                let sub = sub;
                let drop_rx = drop_rx.clone();
                async move {
                    let _sub = sub;
                    let _ = drop_rx.recv_async().await.ok();
                    tracing::info!("dropping subscription");
                }
            });
        }
    }

    filters
}

async fn forward(session: Session, mut link_rx: LinkRx) {
    loop {
        match link_rx.next().await {
            Ok(Some(notif)) => match notif {
                Notification::Forward(forward) => {
                    tracing::debug!(?forward, "mqtt forward notification received");

                    let topic = forward.publish.topic;
                    let payload = forward.publish.payload;

                    tracing::debug!(
                        topic = ?topic,
                        "receiving mqtt publish"
                    );

                    let Ok(topic) = std::str::from_utf8(&topic) else {
                        tracing::warn!("dropping mqtt publish with non-utf8 topic");
                        continue;
                    };
                    let Ok(topic) = zenoh::key_expr::KeyExpr::new(topic) else {
                        tracing::warn!(
                            topic,
                            "dropping mqtt publish with invalid zenoh key expression"
                        );
                        continue;
                    };
                    if topic.is_wild() {
                        tracing::warn!(%topic, "dropping mqtt publish with wildcard topic");
                        continue;
                    }

                    tracing::debug!(%topic, "forwarding mqtt publish to zenoh");

                    let result = session
                        .put(topic, payload)
                        .allowed_destination(Locality::Remote)
                        .await;

                    if let Err(err) = result {
                        tracing::error!(error = %err, "unable to forward mqtt publish to zenoh");
                    }
                }
                _ => {
                    tracing::debug!(?notif, "received unsupported mqtt notification");
                }
            },
            Ok(None) => {
                tracing::warn!("mqtt link yielded no notification");
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
            Err(err) => {
                tracing::error!(error = %err, "mqtt link receive error");
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }
}

async fn spawn_server<P: rumqttd::protocol::Protocol + Clone + Send + 'static>(
    settings: rumqttd::ServerSettings,
    router_tx: rumqttd::RouterTx,
    link_type: rumqttd::LinkType,
    protocol: P,
    drop_rx: DropNotifier,
) {
    let mut server = rumqttd::Server::new(settings, router_tx.clone(), protocol);

    let fut = server.start(link_type);

    let fut = pin!(fut);
    let drop_rx = pin!(drop_rx.into_recv_async());

    match select(fut, drop_rx).await {
        futures_util::future::Either::Left(_) => {
            tracing::info!("mqtt server shutting down");
        }
        futures_util::future::Either::Right(_) => {
            tracing::info!("kill signal received");
        }
    }
}

fn default_router() -> rumqttd::RouterConfig {
    rumqttd::RouterConfig {
        max_connections: 10_000,
        max_outgoing_packet_count: 200,
        max_segment_size: 100 * 1024usize.pow(2), // 100 MiB
        max_segment_count: 10,
        ..Default::default()
    }
}

fn default_v4() -> rumqttd::ServerSettings {
    rumqttd::ServerSettings {
        name: String::from("v4-1"),
        listen: SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 1883),
        tls: None,
        next_connection_delay_ms: 1,
        connections: rumqttd::ConnectionSettings {
            connection_timeout_ms: 60_000,
            max_payload_size: 20480,
            max_inflight_count: 100,
            dynamic_filters: true,
            auth: None,
            external_auth: None,
        },
    }
}

fn default_v5() -> rumqttd::ServerSettings {
    rumqttd::ServerSettings {
        name: String::from("v5-1"),
        listen: SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 1884),
        tls: None,
        next_connection_delay_ms: 1,
        connections: rumqttd::ConnectionSettings {
            connection_timeout_ms: 60_000,
            max_payload_size: 20480,
            max_inflight_count: 100,
            dynamic_filters: false,
            auth: None,
            external_auth: None,
        },
    }
}
