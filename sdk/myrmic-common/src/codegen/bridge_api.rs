use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::time::Duration;

pub use crate::codegen::cell_api::ApiType;
pub use crate::codegen::template::{
    BodyTemplate, ModbusValueTemplate, ParseInto, ResponseHeaderTemplate, TemplateSegments,
};

pub type WireMqttBridge = MqttBridgeRaw<WireMqttIngress, WireMqttEgress>;
pub type UserMqttBridge = MqttBridgeRaw<UserMqttIngress, UserMqttEgress>;

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MqttBridgeRaw<Ingress, Egress> {
    pub name: String,
    #[serde(alias = "broker")]
    pub broker_url: String,

    #[serde(alias = "ingresses")]
    pub ingress: Vec<Ingress>,
    #[serde(alias = "egresses")]
    pub egress: Vec<Egress>,
}

pub type WireMqttIngress = MqttIngressRaw<BodyTemplate>;
pub type UserMqttIngress = MqttIngressRaw<ParseInto<BodyTemplate>>;

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Clone)]
#[serde(deny_unknown_fields)]
pub struct MqttIngressRaw<Body> {
    pub id: String,
    pub topic: String,
    #[serde(default = "Default::default")]
    pub qos: Option<Qos>,
    pub payload: Body,
}

pub type WireMqttEgress = MqttEgressRaw<TemplateSegments, BodyTemplate>;
pub type UserMqttEgress = MqttEgressRaw<ParseInto<TemplateSegments>, ParseInto<BodyTemplate>>;

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Clone)]
#[serde(deny_unknown_fields)]
pub struct MqttEgressRaw<Topic, Body> {
    pub id: String,
    pub topic: Topic,
    #[serde(default = "Default::default")]
    pub qos: Option<Qos>,
    pub payload: Body,
}

pub type WireHttpBridgeApi = HttpBridgeApiRaw<WireHttpEndpoint>;
pub type UserHttpBridgeApi = HttpBridgeApiRaw<UserHttpEndpoint>;

/// `PartialEq`/`Eq` are intentionally *not* derived: `types` now holds a JSON
/// Schema (`schemars::schema::RootSchema`), whose `extensions` map is backed by
/// `serde_json::Value` and is therefore not `Eq`. Nothing compares whole
/// bridge specs for equality, so we simply drop the bounds here.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HttpBridgeApiRaw<Endpoint> {
    pub name: String,
    pub base_url: String,
    /// A JSON Schema document describing the request/response payload types
    /// referenced by the endpoints. Each named type lives under `definitions`
    /// and is turned into a Rust type by `typify` at import time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub types: Option<schemars::schema::RootSchema>,
    pub endpoints: Vec<Endpoint>,
}

pub type WireHttpEndpoint = HttpEndpointRaw<WireHttpRequestTemplate, WireHttpResponseTemplate>;
pub type UserHttpEndpoint = HttpEndpointRaw<UserHttpRequestTemplate, UserHttpResponseTemplate>;

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Clone)]
#[serde(deny_unknown_fields)]
pub struct HttpEndpointRaw<Req, Resp> {
    pub id: String,
    pub request: Req,
    pub response: Resp,
}

pub type WireHttpRequestTemplate =
    HttpRequestTemplateRaw<TemplateSegments, TemplateSegments, TemplateSegments, BodyTemplate>;
pub type UserHttpRequestTemplate = HttpRequestTemplateRaw<
    ParseInto<TemplateSegments>,
    ParseInto<TemplateSegments>,
    ParseInto<TemplateSegments>,
    ParseInto<BodyTemplate>,
>;

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Clone)]
#[serde(deny_unknown_fields)]
pub struct HttpRequestTemplateRaw<Path, Query, Header, Body> {
    pub method: String,
    pub path: Path,
    #[serde(default = "Default::default")]
    pub query: BTreeMap<String, Query>,
    #[serde(default = "Default::default")]
    pub headers: BTreeMap<String, Header>,
    #[serde(default = "Default::default")]
    pub body: Option<Body>,
    #[serde(default = "Default::default")]
    pub timeout_ms: Option<u64>,
}

/// The `response:` block: a map from HTTP status code to the reply shape for that
/// status. The generated `<Endpoint>Reply` enum gets one variant per entry (named
/// by the status's canonical reason) plus an `Unknown(u16)` catch-all.
pub type WireHttpResponseTemplate = BTreeMap<u16, WireHttpResponseVariant>;
pub type UserHttpResponseTemplate = BTreeMap<u16, UserHttpResponseVariant>;

pub type WireHttpResponseVariant = HttpResponseVariantRaw<ResponseHeaderTemplate, BodyTemplate>;

/// The `User` form also accepts the body-string shorthand (`200: "${json:Foo}"`),
/// collapsing (via [`ParseInto`]) to a variant with just that body.
pub type UserHttpResponseVariant =
    ParseInto<HttpResponseVariantRaw<ParseInto<ResponseHeaderTemplate>, ParseInto<BodyTemplate>>>;

/// One status code's reply shape: response headers to surface as fields and an
/// optional body. Neither present yields a unit variant, headers a struct variant,
/// a body alone a tuple variant.
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Clone)]
#[serde(deny_unknown_fields)]
pub struct HttpResponseVariantRaw<Header, Body> {
    #[serde(default = "Default::default")]
    pub headers: BTreeMap<String, Header>,
    #[serde(default = "Default::default")]
    pub body: Option<Body>,
}

/// Parses the body-string shorthand into a bodied, header-less variant. Only the
/// `User` form reaches this (through [`ParseInto`]); the wire form is always the
/// explicit `{ headers, body }` map.
impl<Header, Body> core::str::FromStr for HttpResponseVariantRaw<Header, Body>
where
    Body: core::str::FromStr<Err = String>,
{
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self {
            headers: BTreeMap::new(),
            body: Some(s.parse()?),
        })
    }
}

/// A duration in a bridge spec, written the way people say it: `500ms`, `1s`,
/// `2min`. It stays text in every format, so a spec reads the same in YAML and
/// survives postcard, which is not self-describing, unchanged.
pub mod human_duration {
    use core::time::Duration;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(duration: &Duration, ser: S) -> Result<S::Ok, S::Error> {
        ser.collect_str(&humantime::format_duration(*duration))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Duration, D::Error> {
        let text = String::deserialize(de)?;
        humantime::parse_duration(&text)
            .map_err(|err| serde::de::Error::custom(format!("invalid duration `{text}`: {err}")))
    }

    /// The same for an optional duration.
    pub mod option {
        use core::time::Duration;
        use serde::{Deserialize, Deserializer, Serializer};

        pub fn serialize<S: Serializer>(
            duration: &Option<Duration>,
            ser: S,
        ) -> Result<S::Ok, S::Error> {
            match duration {
                Some(duration) => {
                    ser.serialize_some(&humantime::format_duration(*duration).to_string())
                }
                None => ser.serialize_none(),
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Option<Duration>, D::Error> {
            Option::<String>::deserialize(de)?
                .map(|text| {
                    humantime::parse_duration(&text).map_err(|err| {
                        serde::de::Error::custom(format!("invalid duration `{text}`: {err}"))
                    })
                })
                .transpose()
        }
    }
}

/// The port registered for Modbus TCP.
pub const MODBUS_TCP_PORT: u16 = 502;

fn default_modbus_port() -> u16 {
    MODBUS_TCP_PORT
}

pub type WireModbusBridge = ModbusBridgeRaw<ModbusValueTemplate>;
pub type UserModbusBridge = ModbusBridgeRaw<ParseInto<ModbusValueTemplate>>;

/// A Modbus TCP bridge: one server (a PLC, a coupler, a gateway) and the values the
/// swarm exchanges with it.
///
/// Modbus has no push, so where an MQTT bridge subscribes, a Modbus bridge polls:
///
/// - `poll` entries are read periodically by the bridge and published as events,
/// - `read` entries are read on a cell's request and answered through a callback,
/// - `write` entries are written on a cell's request, without a reply.
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModbusBridgeRaw<Value> {
    pub name: String,
    /// Host name or IP address of the server.
    pub host: String,
    /// TCP port of the server.
    #[serde(default = "default_modbus_port")]
    pub port: u16,
    /// The unit (slave) id every entry addresses unless it sets its own. Defaults
    /// to 255, the id the Modbus TCP spec recommends for devices addressed directly.
    #[serde(default = "Default::default")]
    pub unit_id: Option<u8>,
    /// How long a single request may take before it fails.
    #[serde(default, with = "human_duration::option")]
    pub timeout: Option<Duration>,

    #[serde(default = "Default::default")]
    pub poll: Vec<ModbusPollRaw<Value>>,
    #[serde(default = "Default::default")]
    pub read: Vec<ModbusReadRaw<Value>>,
    #[serde(default = "Default::default")]
    pub write: Vec<ModbusWriteRaw<Value>>,
}

impl UserModbusBridge {
    /// Checks what the types alone cannot express:
    ///
    /// - `bool` values live in the bit tables (coils, discrete inputs), every other
    ///   value in the register tables,
    /// - ids are unique across all entries, since they become the names of the
    ///   generated commands and events,
    /// - a poll interval is not zero,
    /// - the host is not empty.
    pub fn validate(&self) -> Result<(), String> {
        if self.host.trim().is_empty() {
            return Err("modbus bridge: `host` must not be empty".to_owned());
        }

        let polls = self.poll.iter().map(|p| (&p.id, p.register, &p.value.0));
        let reads = self.read.iter().map(|r| (&r.id, r.register, &r.value.0));
        let writes = self
            .write
            .iter()
            .map(|w| (&w.id, w.register.into(), &w.value.0));

        let mut ids = std::collections::BTreeSet::new();
        for (id, register, value) in polls.chain(reads).chain(writes) {
            if !ids.insert(id) {
                return Err(format!("modbus entry id `{id}` is used more than once"));
            }

            let is_bool = matches!(value, ModbusValueTemplate::Bool(_));
            if is_bool != register.holds_bits() {
                return Err(format!(
                    "modbus entry `{id}`: {register:?} holds {}, so its value must {}be `bool`",
                    if register.holds_bits() {
                        "bits"
                    } else {
                        "16-bit registers"
                    },
                    if register.holds_bits() { "" } else { "not " },
                ));
            }
        }

        if let Some(poll) = self.poll.iter().find(|p| p.interval.is_zero()) {
            return Err(format!(
                "modbus poll `{}`: `interval` must be greater than 0",
                poll.id
            ));
        }

        Ok(())
    }
}

pub type WireModbusPoll = ModbusPollRaw<ModbusValueTemplate>;
pub type UserModbusPoll = ModbusPollRaw<ParseInto<ModbusValueTemplate>>;

/// A value the bridge reads every `interval` and publishes as the event `id`.
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Clone)]
#[serde(deny_unknown_fields)]
pub struct ModbusPollRaw<Value> {
    pub id: String,
    pub register: ModbusRegister,
    pub address: u16,
    pub value: Value,
    #[serde(default = "Default::default")]
    pub byte_order: ModbusByteOrder,
    #[serde(default = "Default::default")]
    pub unit_id: Option<u8>,
    #[serde(with = "human_duration")]
    pub interval: Duration,
    /// Publish only when the value differs from the last one published.
    #[serde(default = "Default::default")]
    pub on_change: bool,
}

pub type WireModbusRead = ModbusReadRaw<ModbusValueTemplate>;
pub type UserModbusRead = ModbusReadRaw<ParseInto<ModbusValueTemplate>>;

/// A value a cell reads with the command `id`; the value arrives at its callback.
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Clone)]
#[serde(deny_unknown_fields)]
pub struct ModbusReadRaw<Value> {
    pub id: String,
    pub register: ModbusRegister,
    pub address: u16,
    pub value: Value,
    #[serde(default = "Default::default")]
    pub byte_order: ModbusByteOrder,
    #[serde(default = "Default::default")]
    pub unit_id: Option<u8>,
}

pub type WireModbusWrite = ModbusWriteRaw<ModbusValueTemplate>;
pub type UserModbusWrite = ModbusWriteRaw<ParseInto<ModbusValueTemplate>>;

/// A value a cell writes with the command `id`.
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Clone)]
#[serde(deny_unknown_fields)]
pub struct ModbusWriteRaw<Value> {
    pub id: String,
    pub register: ModbusWritableRegister,
    pub address: u16,
    pub value: Value,
    #[serde(default = "Default::default")]
    pub byte_order: ModbusByteOrder,
    #[serde(default = "Default::default")]
    pub unit_id: Option<u8>,
}

/// One of the four Modbus data tables a bridge entry addresses.
///
/// Coils and discrete inputs hold single bits, input and holding registers hold
/// 16-bit words. Only coils and holding registers are writable, see
/// [`ModbusWritableRegister`].
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ModbusRegister {
    Coil,
    DiscreteInput,
    Input,
    Holding,
}

impl ModbusRegister {
    /// Whether this table holds single bits (coils, discrete inputs) rather than
    /// 16-bit registers.
    pub fn holds_bits(self) -> bool {
        matches!(self, Self::Coil | Self::DiscreteInput)
    }
}

/// The Modbus data tables a bridge can write to. A separate type (instead of a
/// check on [`ModbusRegister`]) turns a `write` entry that targets a read-only
/// table into a parse error.
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ModbusWritableRegister {
    Coil,
    Holding,
}

impl From<ModbusWritableRegister> for ModbusRegister {
    fn from(register: ModbusWritableRegister) -> Self {
        match register {
            ModbusWritableRegister::Coil => Self::Coil,
            ModbusWritableRegister::Holding => Self::Holding,
        }
    }
}

/// How the bytes of a value lie in its registers. Modbus itself only defines
/// 16-bit registers, sent high byte first, so devices disagree on how to lay out
/// anything else. The letters name the bytes of a 32-bit value from the highest,
/// `A`, to the lowest, `D`, in the order they are sent.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ModbusByteOrder {
    /// High word first, high byte first: big-endian, what the Modbus
    /// specification does within a register.
    #[default]
    Abcd,
    /// Low word first, high byte first: the words swapped.
    Cdab,
    /// High word first, low byte first: the bytes in each word swapped.
    Badc,
    /// Low word first, low byte first: little-endian.
    Dcba,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Qos {
    AtMostOnce,
    AtLeastOnce,
    ExactlyOnce,
}

impl TryFrom<u8> for Qos {
    type Error = &'static str;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Qos::AtMostOnce),
            1 => Ok(Qos::AtLeastOnce),
            2 => Ok(Qos::ExactlyOnce),
            _ => Err("invalid QoS number; allowed values are 0, 1, or 2"),
        }
    }
}

impl Serialize for Qos {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Always serialize as a number (u8) for compactness.
        let num = match self {
            Qos::AtMostOnce => 0,
            Qos::AtLeastOnce => 1,
            Qos::ExactlyOnce => 2,
        };
        serializer.serialize_u8(num)
    }
}

impl<'de> Deserialize<'de> for Qos {
    fn deserialize<D>(deserializer: D) -> Result<Qos, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Deserialize a u8 then convert it to Qos using TryFrom.
        let num = u8::deserialize(deserializer)?;
        Qos::try_from(num).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
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

    const MODBUS_SPEC: &str = r#"
name: plc-bridge
host: 192.168.1.50
port: 502
unit_id: 1
timeout: 1s
poll:
  - id: boiler_temperature
    register: input
    address: 100
    value: "${f32:celsius}"
    byte_order: cdab
    interval: 1s
    on_change: true
read:
  - id: read_setpoint
    register: holding
    address: 200
    value: "${i16:setpoint}"
write:
  - id: pump_on
    register: coil
    address: 5
    value: "${bool:on}"
    unit_id: 2
"#;

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
        assert_eq!(poll.interval, Duration::from_secs(1));
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
        let spec = modbus_spec(
            r#"read: [{ id: on, register: holding, address: 0, value: "${bool:on}" }]"#,
        );
        let err = spec.validate().unwrap_err();
        assert!(err.contains("`on`") && err.contains("Holding"), "{err}");
    }

    #[test]
    fn modbus_numbers_must_live_in_a_register_table() {
        let spec = modbus_spec(
            r#"write: [{ id: speed, register: coil, address: 0, value: "${u16:rpm}" }]"#,
        );
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
        assert_eq!(spec.poll[0].interval, Duration::from_millis(250));

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
}
