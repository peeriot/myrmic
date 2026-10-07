use super::*;

#[test]
fn test_deserialize_qos_number() {
    let yaml0 = "0";
    let qos: Qos = serde_yaml::from_str(yaml0).expect("Failed to deserialize 0");
    assert_eq!(qos, Qos::AtMostOnce);

    let yaml1 = "1";
    let qos: Qos = serde_yaml::from_str(yaml1).expect("Failed to deserialize 1");
    assert_eq!(qos, Qos::AtLeastOnce);

    let yaml2 = "2";
    let qos: Qos = serde_yaml::from_str(yaml2).expect("Failed to deserialize 2");
    assert_eq!(qos, Qos::ExactlyOnce);
}

#[test]
fn test_deserialize_invalid_qos() {
    let yaml_invalid = "3";
    let result: Result<Qos, _> = serde_yaml::from_str(yaml_invalid);
    assert!(result.is_err());

    let yaml_invalid = "\"invalid\"";
    let result: Result<Qos, _> = serde_yaml::from_str(yaml_invalid);
    assert!(result.is_err());
}

const MODBUS_SPEC: &str = include_str!("./modbus.yml");

#[test]
fn modbus_spec_parses_all_three_entry_kinds() {
    let spec: UserModbusBridge = serde_yaml::from_str(MODBUS_SPEC).unwrap();

    assert_eq!(spec.name, "plc-bridge");
    assert_eq!(spec.host, "192.168.1.50");
    assert_eq!(spec.port, 502);
    assert_eq!(spec.unit_id, Some(1));
    assert_eq!(spec.timeout, Some(Duration::from_secs(1)));

    let [poll] = spec.poll.as_slice() else {
        panic!("expected one poll entry");
    };
    assert_eq!(poll.id, "boiler_temperature");
    assert_eq!(poll.register, ModbusRegister::Input);
    assert_eq!(poll.address, 100);
    assert_eq!(poll.value.0, ModbusValueTemplate::F32("celsius".into()));
    assert_eq!(poll.byte_order, ModbusByteOrder::Cdab);
    assert_eq!(poll.interval.get(), Duration::from_secs(1));
    assert!(poll.on_change);

    let [read] = spec.read.as_slice() else {
        panic!("expected one read entry");
    };
    assert_eq!(read.register, ModbusRegister::Holding);
    assert_eq!(read.value.0, ModbusValueTemplate::I16("setpoint".into()));

    let [write] = spec.write.as_slice() else {
        panic!("expected one write entry");
    };
    assert_eq!(write.register, ModbusWritableRegister::Coil);
    assert_eq!(write.value.0, ModbusValueTemplate::Bool("on".into()));
    assert_eq!(write.unit_id, Some(2));
}

#[test]
fn modbus_spec_needs_only_a_name_and_a_host() {
    let spec: UserModbusBridge = serde_yaml::from_str("{ name: plc, host: plc }").unwrap();

    assert_eq!(spec.port, 502);
    assert_eq!(spec.unit_id, None);
    assert_eq!(spec.timeout, None);
    assert!(spec.poll.is_empty());
    assert!(spec.read.is_empty());
    assert!(spec.write.is_empty());
}

#[test]
fn modbus_entry_defaults_to_big_endian_and_the_bridge_unit() {
    let spec: UserModbusBridge = serde_yaml::from_str(
        r#"
name: plc
host: plc
poll:
  - { id: counter, register: holding, address: 0, value: "${u32:n}", interval: 500ms }
"#,
    )
    .unwrap();

    let poll = &spec.poll[0];
    assert_eq!(poll.byte_order, ModbusByteOrder::Abcd);
    assert_eq!(poll.unit_id, None);
    assert!(!poll.on_change);
}

#[test]
fn modbus_spec_rejects_unknown_fields() {
    // Also what keeps a Modbus spec from being mistaken for an MQTT or HTTP one.
    let err = serde_yaml::from_str::<UserModbusBridge>(
        "{ name: plc, host: plc, broker_url: 'mqtt://b' }",
    )
    .unwrap_err();
    assert!(err.to_string().contains("unknown field"), "{err}");
}

#[test]
fn modbus_write_to_a_read_only_table_does_not_parse() {
    let result = serde_yaml::from_str::<UserModbusBridge>(
        r#"
name: plc
host: plc
write:
  - { id: nope, register: input, address: 0, value: "${u16:v}" }
"#,
    );
    assert!(result.is_err());
}

fn modbus_spec(entries: &str) -> UserModbusBridge {
    let yaml = format!("name: plc\nhost: plc\n{entries}");
    serde_yaml::from_str(&yaml).unwrap()
}

#[test]
fn modbus_example_spec_is_valid() {
    let spec: UserModbusBridge = serde_yaml::from_str(MODBUS_SPEC).unwrap();
    assert_eq!(spec.validate(), Ok(()));
}

#[test]
fn modbus_bool_must_live_in_a_bit_table() {
    let spec =
        modbus_spec(r#"read: [{ id: on, register: holding, address: 0, value: "${bool:on}" }]"#);
    let err = spec.validate().unwrap_err();
    assert!(err.contains("`on`") && err.contains("Holding"), "{err}");
}

#[test]
fn modbus_numbers_must_live_in_a_register_table() {
    let spec =
        modbus_spec(r#"write: [{ id: speed, register: coil, address: 0, value: "${u16:rpm}" }]"#);
    let err = spec.validate().unwrap_err();
    assert!(err.contains("`speed`") && err.contains("Coil"), "{err}");
}

#[test]
fn modbus_ids_are_unique_across_entry_kinds() {
    let spec = modbus_spec(
        r#"
read: [{ id: setpoint, register: holding, address: 0, value: "${i16:v}" }]
write: [{ id: setpoint, register: holding, address: 0, value: "${i16:v}" }]
"#,
    );
    let err = spec.validate().unwrap_err();
    assert!(err.contains("more than once"), "{err}");
}

#[test]
fn modbus_poll_interval_must_not_be_zero() {
    let spec = modbus_spec(
        r#"poll: [{ id: t, register: input, address: 0, value: "${u16:v}", interval: 0s }]"#,
    );
    let err = spec.validate().unwrap_err();
    assert!(err.contains("interval"), "{err}");
}

#[test]
fn modbus_durations_are_written_with_their_unit() {
    let spec = modbus_spec(
        r#"poll: [{ id: t, register: input, address: 0, value: "${u16:v}", interval: 250ms }]"#,
    );
    assert_eq!(spec.poll[0].interval.get(), Duration::from_millis(250));

    // A bare number would leave the unit to guess.
    let yaml = r#"{ name: p, host: h, poll: [{ id: t, register: input, address: 0, value: "${u16:v}", interval: 250 }] }"#;
    assert!(serde_yaml::from_str::<UserModbusBridge>(yaml).is_err());
}

#[test]
fn modbus_addresses_may_be_written_in_hex() {
    // Device manuals list register addresses in hex.
    let spec = modbus_spec(
        r#"read: [{ id: setpoint, register: holding, address: 0x0200, value: "${u16:v}" }]"#,
    );
    assert_eq!(spec.read[0].address, 512);
}

#[test]
fn modbus_host_must_not_be_empty() {
    let spec: UserModbusBridge = serde_yaml::from_str("{ name: plc, host: '' }").unwrap();
    let err = spec.validate().unwrap_err();
    assert!(err.contains("host"), "{err}");
}

#[test]
fn modbus_registers_use_snake_case_names() {
    let cases = [
        ("coil", ModbusRegister::Coil),
        ("discrete_input", ModbusRegister::DiscreteInput),
        ("input", ModbusRegister::Input),
        ("holding", ModbusRegister::Holding),
    ];

    for (yaml, expected) in cases {
        let register: ModbusRegister = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(register, expected, "{yaml}");
    }
}

#[test]
fn modbus_bit_tables_are_coils_and_discrete_inputs() {
    assert!(ModbusRegister::Coil.holds_bits());
    assert!(ModbusRegister::DiscreteInput.holds_bits());
    assert!(!ModbusRegister::Input.holds_bits());
    assert!(!ModbusRegister::Holding.holds_bits());
}

#[test]
fn modbus_read_only_tables_are_not_writable() {
    for yaml in ["discrete_input", "input"] {
        assert!(serde_yaml::from_str::<ModbusWritableRegister>(yaml).is_err());
    }

    let register: ModbusWritableRegister = serde_yaml::from_str("holding").unwrap();
    assert_eq!(ModbusRegister::from(register), ModbusRegister::Holding);
}

#[test]
fn modbus_byte_order_defaults_to_big_endian() {
    assert_eq!(ModbusByteOrder::default(), ModbusByteOrder::Abcd);

    for (name, order) in [
        ("abcd", ModbusByteOrder::Abcd),
        ("cdab", ModbusByteOrder::Cdab),
        ("badc", ModbusByteOrder::Badc),
        ("dcba", ModbusByteOrder::Dcba),
    ] {
        assert_eq!(
            serde_yaml::from_str::<ModbusByteOrder>(name).unwrap(),
            order
        );
    }
    // The names of the two orders before there were four.
    assert!(serde_yaml::from_str::<ModbusByteOrder>("little").is_err());
}
