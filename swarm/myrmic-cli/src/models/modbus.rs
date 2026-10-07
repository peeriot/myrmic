use sorg_common::ModbusBridge;

pub use myrmic_common::codegen::bridge_api::{
    UserModbusBridge, WireModbusPoll, WireModbusRead, WireModbusWrite,
};

pub fn convert(cell_name: String, bridge: UserModbusBridge) -> anyhow::Result<ModbusBridge> {
    bridge
        .validate()
        .map_err(|err| anyhow::anyhow!("invalid modbus bridge `{cell_name}`: {err}"))?;

    let UserModbusBridge {
        name: _,
        host,
        port,
        unit_id,
        timeout,
        poll,
        read,
        write,
    } = bridge;

    let poll = poll
        .into_iter()
        .map(|v| WireModbusPoll {
            id: v.id,
            register: v.register,
            address: v.address,
            value: v.value.0,
            byte_order: v.byte_order,
            unit_id: v.unit_id,
            interval: v.interval,
            on_change: v.on_change,
        })
        .collect();

    let read = read
        .into_iter()
        .map(|v| WireModbusRead {
            id: v.id,
            register: v.register,
            address: v.address,
            value: v.value.0,
            byte_order: v.byte_order,
            unit_id: v.unit_id,
        })
        .collect();

    let write = write
        .into_iter()
        .map(|v| WireModbusWrite {
            id: v.id,
            register: v.register,
            address: v.address,
            value: v.value.0,
            byte_order: v.byte_order,
            unit_id: v.unit_id,
        })
        .collect();

    Ok(ModbusBridge {
        cell_name,
        host,
        port,
        unit_id,
        timeout,
        poll,
        read,
        write,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sorg_common::{ModbusRegister, ModbusValueTemplate};

    fn spec(yaml: &str) -> UserModbusBridge {
        serde_yaml::from_str(yaml).expect("valid modbus spec")
    }

    #[test]
    fn converts_a_spec_into_the_deploy_form() {
        let bridge = convert(
            "myapp/plc".into(),
            spec(
                r#"
name: plc
host: plc.local
unit_id: 3
poll:
  - { id: temp, register: input, address: 7, value: "${f32:celsius}", interval: 250ms }
"#,
            ),
        )
        .unwrap();

        assert_eq!(bridge.cell_name, "myapp/plc");
        assert_eq!(bridge.host, "plc.local");
        assert_eq!(bridge.port, 502);
        assert_eq!(bridge.unit_id, Some(3));

        let poll = &bridge.poll[0];
        assert_eq!(poll.register, ModbusRegister::Input);
        assert_eq!(poll.address, 7);
        assert_eq!(poll.value, ModbusValueTemplate::F32("celsius".into()));
        assert_eq!(poll.interval, std::time::Duration::from_millis(250));
    }

    #[test]
    fn rejects_an_invalid_spec() {
        let err = convert(
            "myapp/plc".into(),
            spec(
                r#"
name: plc
host: plc.local
read: [{ id: on, register: input, address: 0, value: "${bool:on}" }]
"#,
            ),
        )
        .unwrap_err();

        assert!(err.to_string().contains("invalid modbus bridge"), "{err}");
    }

    #[test]
    fn rejects_an_empty_host() {
        let err = convert("myapp/plc".into(), spec("{ name: plc, host: '' }")).unwrap_err();

        assert!(err.to_string().contains("host"), "{err}");
    }
}
