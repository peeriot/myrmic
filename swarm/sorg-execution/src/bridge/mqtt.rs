use std::sync::Arc;
use std::time::Duration;

use rumqttc::{AsyncClient, EventLoop, QoS, Request, Subscribe};
use sorg_common::{
    BodyTemplate, MqttBridgeDef, MqttBridgeRecord, MqttQos, WireMqttIngress, custom_err,
};
use tokio::sync::{Barrier, Notify};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use zenoh::Session;

use cell_protocol::MailboxCommand;

use crate::Result;
use crate::bridge::consumer::spawn_bridge_command_consumer;
use crate::mqtt::connect_to_broker;
use crate::payload::{Marker, get_val, resolve_segments_as_string};
use crate::wasm::cell::state::DropHandle;

const BARRIER_TIMEOUT: Duration = Duration::from_secs(5);

/// Reconnect delay after a lost broker connection: doubles from the floor up to the cap.
const BACKOFF_FLOOR: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(30);

struct Backoff(Duration);

impl Default for Backoff {
    fn default() -> Self {
        Self(BACKOFF_FLOOR)
    }
}

impl Backoff {
    /// The delay to wait now; the next one is twice as long, up to the cap.
    fn delay(&mut self) -> Duration {
        let delay = self.0;
        self.0 = (delay * 2).min(BACKOFF_CAP);
        delay
    }

    fn reset(&mut self) {
        self.0 = BACKOFF_FLOOR;
    }
}

pub struct MqttBridgeHandle {
    record: MqttBridgeRecord,
    session: Session,
    mailbox_poll_interval: Duration,

    state: BridgeState,
}

enum BridgeState {
    Uninit,
    Init(Inner),
}

struct Inner {
    barrier: Arc<Barrier>,
    kill_signal: Arc<Notify>,
    _tasks: Vec<DropHandle>,
    // The mqtt event-loop poll task that feeds ingress messages is otherwise an
    // untracked `tokio::spawn`: without holding its handle here, it would keep
    // running (and keep publishing cell events) after the bridge is torn down.
    _ingress_tasks: Vec<DropHandle>,
}

impl MqttBridgeHandle {
    pub fn new(
        record: MqttBridgeRecord,
        session: &Session,
        mailbox_poll_interval: Duration,
    ) -> Self {
        Self {
            record,
            session: session.clone(),
            mailbox_poll_interval,
            state: BridgeState::Uninit,
        }
    }

    pub async fn init(&mut self) -> Result<()> {
        if !matches!(self.state, BridgeState::Uninit) {
            Err(custom_err!("init was already called"))?;
        }

        let bridges = std::mem::take(&mut self.record.bridges);

        let barrier = Arc::new(Barrier::new(bridges.len() + 1));
        let kill_signal = Arc::new(Notify::new());

        let db = db_client::v1::Client::new(&self.session);

        let sorg = sorg_client::Client::new(self.session.clone());

        let mut tasks = vec![];
        let mut ingress_tasks = vec![];

        for mut bridge in bridges {
            let (mqtt, event_loop) = connect_to_broker(&bridge.connection).await?;

            for ingress in &bridge.ingress {
                mqtt.subscribe(&ingress.topic, mqtt_qos(ingress.qos))
                    .await
                    .map_err(|err| custom_err!("unable to subscribe to topic: {}", err))?;
            }

            let ingress_task = tokio::spawn({
                let event_loop = event_loop;
                let ingress = std::mem::take(&mut bridge.ingress);
                let sorg = sorg.clone();
                let broker = format!(
                    "{}:{}",
                    bridge.connection.broker_address.as_ref(),
                    bridge.connection.broker_port
                );

                handle_event_loop(event_loop, ingress, sorg, broker)
            });
            ingress_tasks.push(DropHandle::from(ingress_task.abort_handle()));
            ingress_tasks.push(watch_ingress_task(ingress_task, kill_signal.clone()));

            let bridge_sri = cell_protocol::Sri::from_target(&bridge.cell_name)
                .map_err(|e| custom_err!("bridge cell_name '{}' invalid: {e}", bridge.cell_name))?;
            let task = spawn_bridge_command_consumer(
                &db,
                barrier.clone(),
                kill_signal.clone(),
                bridge_sri,
                self.mailbox_poll_interval,
                {
                    let db = db.clone();
                    let bridge = bridge;
                    let mqtt = mqtt;

                    move |command| {
                        let db = db.clone();
                        let mqtt = mqtt.clone();
                        let bridge = bridge.clone();

                        handle_message(db, mqtt, bridge, command)
                    }
                },
            );

            tasks.push(task);
        }

        let _ = timeout(BARRIER_TIMEOUT, barrier.wait())
            .await
            .map_err(|_| custom_err!("unable to start bridge tasks"))?;

        if core::pin::pin!(kill_signal.notified()).enable() {
            Err(custom_err!(
                "there was an error attempting to init the bridge"
            ))?;
        }

        self.state = BridgeState::Init(Inner {
            barrier,
            kill_signal,
            _tasks: tasks,
            _ingress_tasks: ingress_tasks,
        });

        Ok(())
    }

    pub async fn run(&mut self) -> Result<()> {
        let BridgeState::Init(inner) = &mut self.state else {
            return Err(custom_err!("init must be called before run"))?;
        };

        let _ = timeout(BARRIER_TIMEOUT, inner.barrier.wait())
            .await
            .map_err(|_| custom_err!("internal error: mqtt bridge barrier failed"))?;

        let () = inner.kill_signal.notified().await;

        Err(custom_err!("mqtt bridge failed"))?
    }
}

/// Polls the broker connection for ingress messages. A lost connection is retried with
/// backoff (rumqttc reconnects on the next poll); a fresh session gets the ingress
/// subscriptions back, as a clean-session broker has forgotten them.
async fn handle_event_loop(
    mut event_loop: EventLoop,
    ingress: Vec<WireMqttIngress>,
    sorg: sorg_client::Client,
    broker: String,
) {
    let mut backoff = Backoff::default();

    loop {
        // A reconnecting poll that lands a fresh session drops whatever rumqttc parked
        // in `pending` when the connection went: in-flight egress and the channel backlog.
        let reconnecting = event_loop.network.is_none();
        let parked = if reconnecting {
            parked_egress(&event_loop)
        } else {
            0
        };

        let event = match event_loop.poll().await {
            Ok(event) => event,
            Err(err) => {
                let delay = backoff.delay();
                if reconnecting {
                    tracing::warn!(
                        "cannot reach mqtt broker '{broker}', retrying in {delay:?}: {err}"
                    );
                } else {
                    tracing::warn!(
                        "lost the connection to mqtt broker '{broker}', reconnecting in {delay:?}: {err}"
                    );
                }
                tokio::time::sleep(delay).await;
                continue;
            }
        };

        let rumqttc::Event::Incoming(incoming) = event else {
            continue;
        };
        let publish = match incoming {
            rumqttc::Incoming::ConnAck(ack) => {
                tracing::info!("reconnected to mqtt broker '{broker}'");
                backoff.reset();
                if !ack.session_present {
                    if parked > 0 {
                        tracing::warn!(
                            "dropped {parked} egress messages queued before the connection to mqtt broker '{broker}' was lost"
                        );
                    }
                    resubscribe(&mut event_loop, &ingress);
                }
                continue;
            }
            rumqttc::Incoming::Publish(publish) => publish,
            _ => continue,
        };
        let Some(ingress) = ingress.iter().find(|i| i.topic == publish.topic) else {
            continue;
        };

        let event_name = &ingress.id;
        let payload = publish.payload;

        // The cell event is a JSON object keyed by the ingress payload's placeholder
        // name, mirroring the named egress payload the cell sends back.
        let (field_name, value) = match &ingress.payload {
            BodyTemplate::String(name) => {
                let value = match String::from_utf8(payload.to_vec()) {
                    Ok(value) => value,
                    Err(err) => {
                        tracing::warn!("unable to parse text from payload: {}", err);
                        continue;
                    }
                };

                (name, serde_json::Value::String(value))
            }
            BodyTemplate::Json(name) => {
                let value = match serde_json::from_slice::<serde_json::Value>(payload.as_ref()) {
                    Ok(value) => value,
                    Err(err) => {
                        tracing::warn!("unable to parse json from payload: {}", err);
                        continue;
                    }
                };

                (name, value)
            }
            BodyTemplate::Bytes(name) => (name, serde_json::Value::from(payload.to_vec())),
        };

        let mut obj = serde_json::Map::new();
        obj.insert(field_name.clone(), value);

        let payload = match serde_json::to_vec(&serde_json::Value::Object(obj)) {
            Ok(payload) => payload,
            Err(err) => {
                tracing::warn!("unable to serialise ingress payload as json: {}", err);
                continue;
            }
        };

        if let Err(err) = sorg.publish_cell_event(event_name, Some(payload)).await {
            tracing::warn!("unable to publish event: {}", err);
        }
    }
}

fn mqtt_qos(qos: Option<MqttQos>) -> QoS {
    match qos {
        Some(MqttQos::ExactlyOnce) => QoS::ExactlyOnce,
        Some(MqttQos::AtLeastOnce) => QoS::AtLeastOnce,
        _ => QoS::AtMostOnce,
    }
}

/// Egress publishes rumqttc parked in `pending` when the connection went.
fn parked_egress(event_loop: &EventLoop) -> usize {
    event_loop
        .pending
        .iter()
        .filter(|request| matches!(request, Request::Publish(_)))
        .count()
}

/// Queues a subscribe for every ingress topic ahead of any buffered egress. The event
/// loop task is the request channel's only consumer, so it must not block on that
/// channel; `pending` is drained before it.
fn resubscribe(event_loop: &mut EventLoop, ingress: &[WireMqttIngress]) {
    for ingress in ingress {
        let subscribe = Subscribe::new(&ingress.topic, mqtt_qos(ingress.qos));
        event_loop.pending.push_back(Request::Subscribe(subscribe));
    }
}

/// Fires `kill_signal` once the ingress task ends, returned or panicked, so the bridge
/// fails visibly instead of looking alive with no event loop behind it.
fn watch_ingress_task(task: JoinHandle<()>, kill_signal: Arc<Notify>) -> DropHandle {
    DropHandle::from(tokio::spawn(async move {
        let _ = task.await;
        kill_signal.notify_one();
    }))
}

#[allow(clippy::too_many_lines)] // It's called art
async fn handle_message(
    db: db_client::v1::Client,
    mqtt: AsyncClient,
    bridge: MqttBridgeDef,
    command: MailboxCommand,
) -> Result<()> {
    let MailboxCommand {
        cmd,
        payload,
        attachment: _,
    } = command;

    tracing::debug!("Attempting to process: {}", cmd.as_ref());

    let Some(egress) = bridge.egress.iter().find(|t| t.id == cmd.as_ref()) else {
        return Err(custom_err!("unknown command: {}", cmd.as_ref()))?;
    };

    tracing::debug!("found egress for {}", cmd.as_ref());

    let payload = payload.unwrap_or_default();

    tracing::debug!("payload length: {}", payload.len());

    let qos = mqtt_qos(egress.qos);

    let egress = egress.clone();

    let vals = {
        let mut markers = Marker::collect(&egress.topic);
        markers.push(Marker::from_body_template(&egress.payload));

        tracing::debug!("expecting {} fields", markers.len());

        let obj = crate::payload::parse_payload_object(payload.as_slice())?;
        crate::payload::decode_vals(markers, obj)?
    };

    let topic = resolve_segments_as_string(&db, egress.topic, &vals).await?;

    match egress.payload {
        BodyTemplate::String(name) => {
            let value = get_val(&vals, &name)?
                .into_string()
                .map_err(|err| custom_err!("unable to convert to string: {}", err))?;

            mqtt.publish(topic, qos, false, value)
                .await
                .map_err(|err| custom_err!("unable to publish to mqtt: {}", err))?;
        }
        BodyTemplate::Json(name) => {
            let value = get_val(&vals, &name)?
                .into_json()
                .map_err(|err| custom_err!("unable to convert to json: {}", err))?;

            let value = serde_json::to_vec(&value)
                .map_err(|err| custom_err!("unable to convert to json: {}", err))?;

            mqtt.publish_bytes(topic, qos, false, value.into())
                .await
                .map_err(|err| custom_err!("unable to publish to mqtt: {}", err))?;
        }
        BodyTemplate::Bytes(name) => {
            let value = get_val(&vals, &name)?
                .into_bytes()
                .map_err(|err| custom_err!("unable to convert to bytes: {}", err))?;

            mqtt.publish_bytes(topic, qos, false, value.into())
                .await
                .map_err(|err| custom_err!("unable to publish to mqtt: {}", err))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use rumqttc::{MqttOptions, Publish};

    use super::*;

    #[test]
    fn parked_egress_counts_only_the_publishes() {
        let mut event_loop = EventLoop::new(MqttOptions::new("test", "localhost", 1883), 16);
        assert_eq!(parked_egress(&event_loop), 0);

        event_loop
            .pending
            .push_back(Request::Subscribe(Subscribe::new(
                "ingress",
                QoS::AtMostOnce,
            )));
        event_loop.pending.push_back(Request::Publish(Publish::new(
            "egress",
            QoS::AtMostOnce,
            "one",
        )));
        event_loop.pending.push_back(Request::Publish(Publish::new(
            "egress",
            QoS::AtLeastOnce,
            "two",
        )));
        assert_eq!(parked_egress(&event_loop), 2);
    }

    #[test]
    fn reconnect_backoff_doubles_to_the_cap_and_resets() {
        let mut backoff = Backoff::default();
        assert_eq!(backoff.delay(), Duration::from_secs(1));
        assert_eq!(backoff.delay(), Duration::from_secs(2));
        assert_eq!(backoff.delay(), Duration::from_secs(4));
        assert_eq!(backoff.delay(), Duration::from_secs(8));
        assert_eq!(backoff.delay(), Duration::from_secs(16));
        assert_eq!(backoff.delay(), Duration::from_secs(30));
        assert_eq!(backoff.delay(), Duration::from_secs(30));
        backoff.reset();
        assert_eq!(backoff.delay(), Duration::from_secs(1));
    }

    #[tokio::test]
    async fn ingress_task_ending_fires_the_kill_signal() {
        let kill_signal = Arc::new(Notify::new());
        let _watcher = watch_ingress_task(tokio::spawn(async {}), kill_signal.clone());
        timeout(Duration::from_secs(1), kill_signal.notified())
            .await
            .expect("the kill signal should fire once the ingress task returns");
    }

    #[tokio::test]
    async fn ingress_task_panicking_fires_the_kill_signal() {
        let kill_signal = Arc::new(Notify::new());
        let _watcher = watch_ingress_task(
            tokio::spawn(async { panic!("ingress task died") }),
            kill_signal.clone(),
        );
        timeout(Duration::from_secs(1), kill_signal.notified())
            .await
            .expect("the kill signal should fire once the ingress task panics");
    }
}
