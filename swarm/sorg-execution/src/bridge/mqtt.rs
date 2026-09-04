use std::sync::Arc;
use std::time::Duration;

use rumqttc::{AsyncClient, EventLoop, QoS};
use sorg_common::{
    BodyTemplate, MqttBridgeDef, MqttBridgeRecord, MqttQos, WireMqttIngress, custom_err,
};
use tokio::sync::{Barrier, Notify};
use tokio::time::timeout;
use zenoh::Session;

use cell_protocol::MailboxCommand;

use crate::Result;
use crate::bridge::consumer::spawn_bridge_command_consumer;
use crate::mqtt::connect_to_broker;
use crate::payload::{Marker, get_val, resolve_segments_as_string};
use crate::wasm::cell::state::DropHandle;

const BARRIER_TIMEOUT: Duration = Duration::from_secs(5);

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
                let topic = &ingress.topic;

                let qos = match ingress.qos {
                    Some(MqttQos::ExactlyOnce) => QoS::ExactlyOnce,
                    Some(MqttQos::AtLeastOnce) => QoS::AtLeastOnce,
                    _ => QoS::AtMostOnce,
                };

                mqtt.subscribe(topic, qos)
                    .await
                    .map_err(|err| custom_err!("unable to subscribe to topic: {}", err))?;
            }

            let ingress_task = tokio::spawn({
                let event_loop = event_loop;
                let ingress = std::mem::take(&mut bridge.ingress);
                let sorg = sorg.clone();

                handle_event_loop(event_loop, ingress, sorg)
            });
            ingress_tasks.push(DropHandle::from(ingress_task));

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

async fn handle_event_loop(
    mut event_loop: EventLoop,
    ingress: Vec<WireMqttIngress>,
    sorg: sorg_client::Client,
) {
    loop {
        let event = event_loop.poll().await.unwrap();

        let rumqttc::Event::Incoming(incoming) = event else {
            continue;
        };
        let rumqttc::Incoming::Publish(publish) = incoming else {
            continue;
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

    let qos = match egress.qos {
        Some(MqttQos::ExactlyOnce) => QoS::ExactlyOnce,
        Some(MqttQos::AtLeastOnce) => QoS::AtLeastOnce,
        _ => QoS::AtMostOnce,
    };

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
