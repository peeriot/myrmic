//! `poll` entries: the bridge reads a value periodically and publishes it as an
//! event, which takes the place of the subscription an MQTT bridge would have.

use std::future::Future;

use sorg_common::WireModbusPoll;
use tokio::time::MissedTickBehavior;

use super::{codec::ModbusValue, connection::Connection, point::Point};

/// Polls `entry` until the returned future is dropped: reads it every
/// `interval` and hands the event name and payload to `publish`.
///
/// The payload is a JSON object with the value under its field name, e.g.
/// `{"celsius": 21.5}` for `${f32:celsius}`, the same shape an MQTT bridge uses
/// for its ingress events.
pub(crate) async fn poll<P, F>(
    connection: Connection,
    entry: WireModbusPoll,
    default_unit: u8,
    mut publish: P,
) where
    P: FnMut(String, Vec<u8>) -> F,
    F: Future<Output = ()>,
{
    let point = Point::of_poll(&entry, default_unit);

    let mut interval = tokio::time::interval(entry.interval.get());
    // A slow device must not cause a burst of reads once it answers again.
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

    let mut last_published = None;
    let mut failing = false;

    loop {
        interval.tick().await;

        let value = match point.read(&connection).await {
            Ok(value) => value,
            Err(err) => {
                // Log the first failure, not every single poll of a device that is down.
                if !failing {
                    tracing::warn!("modbus poll `{}` failed: {err}", entry.id);
                    failing = true;
                }
                continue;
            }
        };

        if failing {
            tracing::info!("modbus poll `{}` succeeds again", entry.id);
            failing = false;
        }

        if entry.on_change && last_published == Some(value) {
            continue;
        }

        publish(entry.id.clone(), payload(&point, value)).await;
        last_published = Some(value);
    }
}

fn payload<R>(point: &Point<R>, value: ModbusValue) -> Vec<u8> {
    let mut obj = serde_json::Map::new();
    obj.insert(point.template.name().to_owned(), value.to_json());

    serde_json::Value::Object(obj).to_string().into_bytes()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use myrmic_common::human_duration::NonZeroDuration;
    use sorg_common::{ModbusByteOrder, ModbusRegister};
    use tokio::sync::mpsc;

    use super::*;
    use crate::wasm::cell::state::DropHandle;
    use sorg_tests::ModbusMockHandle;

    /// Long enough for any event that is going to arrive.
    const PATIENCE: Duration = Duration::from_secs(5);

    fn entry(on_change: bool) -> WireModbusPoll {
        WireModbusPoll {
            id: "temperature".to_owned(),
            register: ModbusRegister::Input,
            address: 1,
            value: "${i16:celsius}".parse().unwrap(),
            byte_order: ModbusByteOrder::Abcd,
            unit_id: None,
            interval: NonZeroDuration::new(Duration::from_millis(10)).unwrap(),
            on_change,
        }
    }

    /// Starts polling `entry` on `server`; the events arrive on the receiver.
    fn start(
        server: &ModbusMockHandle,
        entry: WireModbusPoll,
    ) -> (
        mpsc::UnboundedReceiver<(String, serde_json::Value)>,
        [DropHandle; 2],
    ) {
        let (connection, connection_task) = Connection::spawn(server.address(), PATIENCE);
        let (events, rx) = mpsc::unbounded_channel();

        let task = tokio::spawn(poll(connection, entry, 1, move |event, payload| {
            let payload = serde_json::from_slice(&payload).unwrap();
            events.send((event, payload)).unwrap();
            async {}
        }));

        (rx, [connection_task, DropHandle::from(task)])
    }

    async fn next(
        rx: &mut mpsc::UnboundedReceiver<(String, serde_json::Value)>,
    ) -> (String, serde_json::Value) {
        tokio::time::timeout(PATIENCE, rx.recv())
            .await
            .expect("no event arrived")
            .unwrap()
    }

    #[tokio::test]
    async fn publishes_the_value_as_an_event_on_every_poll() {
        let server = ModbusMockHandle::start().await;
        server.set_input_registers(1, &[21]);

        let (mut rx, _tasks) = start(&server, entry(false));

        let expected = ("temperature".to_owned(), serde_json::json!({"celsius": 21}));
        assert_eq!(next(&mut rx).await, expected);
        assert_eq!(next(&mut rx).await, expected);
    }

    #[tokio::test]
    async fn on_change_publishes_only_changed_values() {
        let server = ModbusMockHandle::start().await;
        server.set_input_registers(1, &[21]);

        let (mut rx, _tasks) = start(&server, entry(true));
        assert_eq!(next(&mut rx).await.1, serde_json::json!({"celsius": 21}));

        // Many polls of the same value go by without an event...
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(rx.try_recv().is_err());

        // ...until it changes.
        server.set_input_registers(1, &[22]);
        assert_eq!(next(&mut rx).await.1, serde_json::json!({"celsius": 22}));
    }

    #[tokio::test]
    async fn keeps_polling_after_a_failed_read() {
        let server = ModbusMockHandle::start().await;
        // Nothing at address 1 yet: every read fails with IllegalDataAddress.
        let (mut rx, _tasks) = start(&server, entry(false));

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(rx.try_recv().is_err());

        server.set_input_registers(1, &[(-4_i16).cast_unsigned()]);
        assert_eq!(next(&mut rx).await.1, serde_json::json!({"celsius": -4}));
    }
}
