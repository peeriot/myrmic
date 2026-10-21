//! Commands a cell sends to the bridge: `write` and `read` entries.

use sorg_common::{ModbusValueTemplate, WireModbusRead, WireModbusWrite, custom_err};

use super::codec::ModbusValue;
use super::connection::{Connection, Error};
use super::point::Point;
use crate::bridge::reply;

/// The answer to a `read` command, for the callback the calling cell named.
#[derive(Debug, PartialEq)]
pub(crate) struct Reply {
    pub callback: String,
    pub payload: Vec<u8>,
}

/// Handles the command of a `read` entry: reads the entry's point and returns
/// the reply for the caller's callback. A caller that named no callback gets
/// nothing, so the device is not even asked.
///
/// The command payload carries nothing but the callback. The reply is the JSON of
/// one of three outcomes, in the externally tagged form serde gives the generated
/// reply enum:
///
/// - `{"Ok": {"setpoint": 215}}`: the value, under its field name,
/// - `{"Exception": 2}`: the device refused, with the Modbus exception code,
/// - `{"Failed": "..."}`: no (usable) answer from the device.
pub(crate) async fn read(
    connection: &Connection,
    entry: &WireModbusRead,
    default_unit: u8,
    payload: &[u8],
) -> crate::Result<Option<Reply>> {
    let mut obj = crate::payload::parse_payload_object(payload)?;
    let callback = reply::take_callback(&mut obj)?;
    if let Some(unknown) = obj.keys().next() {
        Err(custom_err!("unknown field `{unknown}` in payload"))?;
    }

    let Some(callback) = callback else {
        tracing::debug!("modbus read `{}` names no callback; skipped", entry.id);
        return Ok(None);
    };

    let point = Point::of_read(entry, default_unit);
    let outcome = point.read(connection).await;

    Ok(Some(Reply {
        callback,
        payload: read_reply(&point.template, outcome)
            .to_string()
            .into_bytes(),
    }))
}

fn read_reply(
    template: &ModbusValueTemplate,
    outcome: Result<ModbusValue, Error>,
) -> serde_json::Value {
    match outcome {
        Ok(value) => serde_json::json!({ "Ok": { template.name(): value.to_json() } }),
        Err(Error::Exception(code)) => serde_json::json!({ "Exception": u8::from(code) }),
        Err(err @ (Error::Failed(_) | Error::Unexpected(_))) => {
            serde_json::json!({ "Failed": err.to_string() })
        }
    }
}

/// Handles the command of a `write` entry: takes the value from the command
/// payload and writes it to the entry's point.
///
/// The payload is a JSON object with the value under its field name, e.g.
/// `{"on": true}` for `${bool:on}`.
pub(crate) async fn write(
    connection: &Connection,
    entry: &WireModbusWrite,
    default_unit: u8,
    payload: &[u8],
) -> crate::Result<()> {
    let point = Point::of_write(entry, default_unit);
    let value = value_from_payload(&point.template, payload)?;

    point
        .write(connection, value)
        .await
        .map_err(|err| custom_err!("modbus write `{}` failed: {err}", entry.id))?;

    Ok(())
}

fn value_from_payload(
    template: &ModbusValueTemplate,
    payload: &[u8],
) -> crate::Result<ModbusValue> {
    let mut obj = crate::payload::parse_payload_object(payload)?;

    let name = template.name();
    let Some(json) = obj.remove(name) else {
        return Err(custom_err!("missing field `{name}` in payload"))?;
    };
    if let Some(unknown) = obj.keys().next() {
        Err(custom_err!("unknown field `{unknown}` in payload"))?;
    }

    Ok(ModbusValue::from_json(template, json)
        .map_err(|err| custom_err!("field `{name}`: {err}"))?)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sorg_common::{
        ModbusByteOrder, ModbusRegister, ModbusServerAddress, ModbusWritableRegister,
    };

    use super::*;
    use sorg_tests::{ModbusMockHandle, SilentModbusServer};

    fn template(s: &str) -> ModbusValueTemplate {
        s.parse().unwrap()
    }

    #[test]
    fn takes_the_value_from_its_field() {
        let value = value_from_payload(&template("${u16:rpm}"), br#"{"rpm": 1500}"#);
        assert_eq!(value.unwrap(), ModbusValue::U16(1500));
    }

    #[test]
    fn rejects_payloads_that_do_not_match_the_template() {
        let cases = [
            (&b"{}"[..], "missing field `rpm`"),
            (br#"{"rpm": 1500, "extra": 1}"#, "unknown field `extra`"),
            (br#"{"rpm": -1}"#, "field `rpm`"),
            (b"[1500]", "object"),
        ];

        for (payload, expected) in cases {
            let err = value_from_payload(&template("${u16:rpm}"), payload).unwrap_err();
            assert!(err.to_string().contains(expected), "{err}");
        }
    }

    fn read_entry(address: u16) -> WireModbusRead {
        WireModbusRead {
            id: "read_setpoint".to_owned(),
            register: ModbusRegister::Holding,
            address,
            value: template("${i16:setpoint}"),
            byte_order: ModbusByteOrder::Abcd,
            unit_id: None,
        }
    }

    async fn reply(
        server: ModbusServerAddress,
        entry: &WireModbusRead,
        payload: &[u8],
    ) -> Option<(String, serde_json::Value)> {
        let (connection, _task) = Connection::spawn(server, Duration::from_millis(500));
        let reply = read(&connection, entry, 1, payload).await.unwrap()?;

        Some((
            reply.callback,
            serde_json::from_slice(&reply.payload).unwrap(),
        ))
    }

    const CALLBACK: &[u8] = br#"{"__callback": "setpoint_read"}"#;

    #[tokio::test]
    async fn a_read_replies_the_value_to_the_callback() {
        let server = ModbusMockHandle::start().await;
        server.set_holding_registers(3, &[215]);

        assert_eq!(
            reply(server.address(), &read_entry(3), CALLBACK).await,
            Some((
                "setpoint_read".to_owned(),
                serde_json::json!({"Ok": {"setpoint": 215}})
            ))
        );
    }

    #[tokio::test]
    async fn a_read_replies_the_exception_code_of_a_refusing_device() {
        let server = ModbusMockHandle::start().await;

        // Nothing at address 3: IllegalDataAddress, exception code 2.
        let (_, reply) = reply(server.address(), &read_entry(3), CALLBACK)
            .await
            .unwrap();
        assert_eq!(reply, serde_json::json!({"Exception": 2}));
    }

    #[tokio::test]
    async fn a_read_replies_failed_when_the_device_does_not_answer() {
        let server = SilentModbusServer::start().await;
        let (_, reply) = reply(server.address(), &read_entry(3), CALLBACK)
            .await
            .unwrap();
        assert!(reply["Failed"].is_string(), "{reply}");
    }

    #[tokio::test]
    async fn a_read_without_callback_is_skipped() {
        let server = ModbusMockHandle::start().await;
        server.set_holding_registers(3, &[215]);

        assert_eq!(reply(server.address(), &read_entry(3), b"{}").await, None);
    }

    #[tokio::test]
    async fn a_read_takes_no_fields_but_the_callback() {
        let server = ModbusMockHandle::start().await;
        let (connection, _task) = Connection::spawn(server.address(), Duration::from_secs(5));

        let payload = br#"{"__callback": "setpoint_read", "address": 4}"#;
        let err = read(&connection, &read_entry(3), 1, payload)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unknown field `address`"), "{err}");
    }

    #[tokio::test]
    async fn writes_the_commanded_value() {
        let server = ModbusMockHandle::start().await;
        let (connection, _task) = Connection::spawn(server.address(), Duration::from_secs(5));
        let entry = WireModbusWrite {
            id: "pump_on".to_owned(),
            register: ModbusWritableRegister::Coil,
            address: 5,
            value: template("${bool:on}"),
            byte_order: ModbusByteOrder::Abcd,
            unit_id: None,
        };

        write(&connection, &entry, 1, br#"{"on": true}"#)
            .await
            .unwrap();

        assert_eq!(server.coil(5), Some(true));
    }
}
