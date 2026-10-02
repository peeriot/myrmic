//! Wire-level bridge configuration types shared by the cell deploy path (`CellConfig::HttpBridge`/
//! `MqttBridge`) and the myrmic-cli build/nest tooling that produces them.

use crate::MqttConnection;
use serde::{Deserialize, Serialize};

pub use myrmic_common::codegen::bridge_api::{
    ModbusByteOrder, ModbusRegister, ModbusWritableRegister, WireHttpEndpoint,
    WireHttpRequestTemplate, WireHttpResponseTemplate, WireHttpResponseVariant, WireModbusPoll,
    WireModbusRead, WireModbusWrite, WireMqttEgress, WireMqttIngress,
};
pub use myrmic_common::codegen::status::status_variant_name;
pub use myrmic_common::codegen::template::{
    BodyTemplate, ModbusValueTemplate, ResponseHeaderTemplate, TemplateSegment, TemplateSegments,
};

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpBridgeConfig {
    pub api: Vec<HttpBridgeApi>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
pub struct HttpBridgeApi {
    pub cell_name: String,
    pub base_url: String,
    pub endpoints: Vec<WireHttpEndpoint>,
}

/// The resolved form of one or more [`HttpBridgeApi`]s, as consumed by
/// `sorg_execution::bridge::http::HttpBridgeHandle` to spawn the egress mailbox tasks.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
pub struct HttpBridgeRecord {
    pub api: Vec<HttpBridgeApi>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MqttBridgeConfig {
    pub bridges: Vec<MqttBridge>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
pub struct MqttBridge {
    pub cell_name: String,
    pub broker: String,
    pub egress: Vec<WireMqttEgress>,
    pub ingress: Vec<WireMqttIngress>,
}

/// The resolved form of one or more [`MqttBridge`]s, as consumed by
/// `sorg_execution::bridge::mqtt::MqttBridgeHandle` to spawn the mailbox tasks; unlike
/// [`MqttBridge`], the broker address is already parsed into a connection descriptor.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
pub struct MqttBridgeRecord {
    pub bridges: Vec<MqttBridgeDef>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
pub struct MqttBridgeDef {
    pub cell_name: String,

    pub connection: MqttConnection,

    pub egress: Vec<WireMqttEgress>,
    pub ingress: Vec<WireMqttIngress>,
}

/// A Modbus bridge as the deploy path carries it: the spec's entries with their
/// `${type:name}` templates already parsed, and the bridge named by its cell.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
pub struct ModbusBridge {
    pub cell_name: String,
    pub host: String,
    pub port: u16,
    pub unit_id: Option<u8>,
    #[serde(with = "myrmic_common::codegen::bridge_api::human_duration::option")]
    pub timeout: Option<std::time::Duration>,

    pub poll: Vec<WireModbusPoll>,
    pub read: Vec<WireModbusRead>,
    pub write: Vec<WireModbusWrite>,
}

impl ModbusBridge {
    /// The server the bridge connects to.
    pub fn server_address(&self) -> ModbusServerAddress {
        ModbusServerAddress {
            host: self.host.clone(),
            port: self.port,
        }
    }
}

/// The host and port of a Modbus TCP server. The host stays a name here; it is
/// resolved when the bridge connects.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct ModbusServerAddress {
    pub host: String,
    pub port: u16,
}

impl core::fmt::Display for ModbusServerAddress {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // An IPv6 address is bracketed, so its colons are not taken for the port.
        if self.host.contains(':') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ModbusServerAddress;

    fn address(host: &str, port: u16) -> String {
        ModbusServerAddress {
            host: host.to_owned(),
            port,
        }
        .to_string()
    }

    #[test]
    fn modbus_server_address_shows_host_and_port() {
        assert_eq!(address("plc.local", 502), "plc.local:502");
        assert_eq!(address("192.168.1.50", 5020), "192.168.1.50:5020");
    }

    #[test]
    fn modbus_server_address_brackets_an_ipv6_host() {
        assert_eq!(address("fe80::1", 1502), "[fe80::1]:1502");
    }
}
