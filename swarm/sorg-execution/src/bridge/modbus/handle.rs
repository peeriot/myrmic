//! The bridge cell: wires the connection, the pollers and the command mailbox
//! together, following the same `init`/`run` protocol as the MQTT and HTTP bridges.

use std::sync::Arc;
use std::time::Duration;

use cell_protocol::MailboxCommand;
use sorg_common::{ModbusBridge, custom_err};
use tokio::sync::{Barrier, Notify};
use tokio::time::timeout;
use zenoh::Session;

use super::command::{self, Reply};
use super::connection::Connection;
use super::poll::poll;
use crate::bridge::consumer::spawn_bridge_command_consumer;
use crate::bridge::reply;
use crate::wasm::cell::state::DropHandle;

const BARRIER_TIMEOUT: Duration = Duration::from_secs(5);

/// Used when the spec sets no `timeout`.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);

/// Used when the spec sets no `unit_id`: the unit id the Modbus TCP spec
/// recommends for a device that is addressed directly, not through a gateway.
const DEFAULT_UNIT: u8 = 255;

pub struct ModbusBridgeHandle {
    bridge: Arc<ModbusBridge>,
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
    // The connection, the pollers and the command consumer: dropping the handle
    // stops them all.
    _tasks: Vec<DropHandle>,
}

impl ModbusBridgeHandle {
    pub fn new(bridge: ModbusBridge, session: &Session, mailbox_poll_interval: Duration) -> Self {
        Self {
            bridge: Arc::new(bridge),
            session: session.clone(),
            mailbox_poll_interval,
            state: BridgeState::Uninit,
        }
    }

    pub async fn init(&mut self) -> crate::Result<()> {
        if !matches!(self.state, BridgeState::Uninit) {
            Err(custom_err!("init was already called"))?;
        }

        let bridge = self.bridge.clone();
        let server = bridge.server_address();
        let bridge_sri = cell_protocol::Sri::from_target(&bridge.cell_name)
            .map_err(|e| custom_err!("bridge cell_name '{}' invalid: {e}", bridge.cell_name))?;

        let request_timeout = bridge.timeout.unwrap_or(DEFAULT_TIMEOUT);
        let unit = bridge.unit_id.unwrap_or(DEFAULT_UNIT);

        let (connection, connection_task) = Connection::spawn(server, request_timeout);
        let mut tasks = vec![connection_task];

        let sorg = sorg_client::Client::new(self.session.clone());
        for entry in &bridge.poll {
            let sorg = sorg.clone();
            let publish = move |event: String, payload: Vec<u8>| {
                let sorg = sorg.clone();
                async move {
                    tracing::debug!(
                        event,
                        payload = %String::from_utf8_lossy(&payload),
                        "modbus event"
                    );
                    if let Err(err) = sorg.publish_cell_event(&event, Some(payload)).await {
                        tracing::warn!("unable to publish modbus event `{event}`: {err}");
                    }
                }
            };
            let task = tokio::spawn(poll(connection.clone(), entry.clone(), unit, publish));
            tasks.push(DropHandle::from(task));
        }

        // One command consumer, plus this function, meet at the barrier.
        let barrier = Arc::new(Barrier::new(2));
        let kill_signal = Arc::new(Notify::new());
        let db = db_client::v1::Client::new(&self.session);

        let consumer = spawn_bridge_command_consumer(
            &db,
            barrier.clone(),
            kill_signal.clone(),
            bridge_sri,
            self.mailbox_poll_interval,
            {
                let db = db.clone();

                move |command| {
                    let db = db.clone();
                    let connection = connection.clone();
                    let bridge = bridge.clone();

                    handle_command(db, connection, bridge, unit, command)
                }
            },
        );
        tasks.push(consumer);

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
        });

        Ok(())
    }

    pub async fn run(&mut self) -> crate::Result<()> {
        let BridgeState::Init(inner) = &mut self.state else {
            return Err(custom_err!("init must be called before run"))?;
        };

        let _ = timeout(BARRIER_TIMEOUT, inner.barrier.wait())
            .await
            .map_err(|_| custom_err!("internal error: modbus bridge barrier failed"))?;

        let () = inner.kill_signal.notified().await;

        Err(custom_err!("modbus bridge failed"))?
    }
}

async fn handle_command(
    db: db_client::v1::Client,
    connection: Connection,
    bridge: Arc<ModbusBridge>,
    unit: u8,
    command: MailboxCommand,
) -> crate::Result<()> {
    let MailboxCommand {
        cmd,
        payload,
        attachment,
    } = command;
    let payload = payload.unwrap_or_default();

    let reply = dispatch(&connection, &bridge, unit, cmd.as_ref(), &payload).await?;

    // A reply can only go back if the command says who sent it.
    let (Some(Reply { callback, payload }), Some(sender)) = (reply, attachment.sender()) else {
        return Ok(());
    };

    reply::deliver(&db, &bridge.cell_name, sender, callback, payload).await
}

/// Runs the entry the command `name` stands for, and returns the reply for the
/// caller if it is a `read` entry.
async fn dispatch(
    connection: &Connection,
    bridge: &ModbusBridge,
    unit: u8,
    name: &str,
    payload: &[u8],
) -> crate::Result<Option<Reply>> {
    if let Some(entry) = bridge.write.iter().find(|w| w.id == name) {
        command::write(connection, entry, unit, payload).await?;
        return Ok(None);
    }

    if let Some(entry) = bridge.read.iter().find(|r| r.id == name) {
        return command::read(connection, entry, unit, payload).await;
    }

    Err(custom_err!("unknown command: {name}"))?
}

#[cfg(test)]
mod tests {
    use sorg_common::{
        ModbusByteOrder, ModbusRegister, ModbusWritableRegister, WireModbusRead, WireModbusWrite,
    };

    use super::*;
    use sorg_tests::ModbusMockHandle;

    fn bridge(server: &ModbusMockHandle) -> ModbusBridge {
        ModbusBridge {
            cell_name: "test/plc".to_owned(),
            host: server.address().host,
            port: server.address().port,
            unit_id: None,
            timeout: None,
            poll: vec![],
            read: vec![WireModbusRead {
                id: "read_setpoint".to_owned(),
                register: ModbusRegister::Holding,
                address: 1,
                value: "${u16:setpoint}".parse().unwrap(),
                byte_order: ModbusByteOrder::Abcd,
                unit_id: None,
            }],
            write: vec![WireModbusWrite {
                id: "set_setpoint".to_owned(),
                register: ModbusWritableRegister::Holding,
                address: 1,
                value: "${u16:setpoint}".parse().unwrap(),
                byte_order: ModbusByteOrder::Abcd,
                unit_id: None,
            }],
        }
    }

    #[tokio::test]
    async fn dispatches_commands_to_their_entries() {
        let server = ModbusMockHandle::start().await;
        let bridge = bridge(&server);
        let (connection, _task) = Connection::spawn(server.address(), DEFAULT_TIMEOUT);

        let written = dispatch(
            &connection,
            &bridge,
            DEFAULT_UNIT,
            "set_setpoint",
            br#"{"setpoint": 42}"#,
        )
        .await
        .unwrap();
        assert_eq!(written, None);

        let reply = dispatch(
            &connection,
            &bridge,
            DEFAULT_UNIT,
            "read_setpoint",
            br#"{"__callback": "got_setpoint"}"#,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(reply.callback, "got_setpoint");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&reply.payload).unwrap(),
            serde_json::json!({"Ok": {"setpoint": 42}})
        );
    }

    #[tokio::test]
    async fn rejects_a_command_no_entry_stands_for() {
        let server = ModbusMockHandle::start().await;
        let bridge = bridge(&server);
        let (connection, _task) = Connection::spawn(server.address(), DEFAULT_TIMEOUT);

        let err = dispatch(&connection, &bridge, DEFAULT_UNIT, "reboot", b"{}")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unknown command"), "{err}");
    }
}
