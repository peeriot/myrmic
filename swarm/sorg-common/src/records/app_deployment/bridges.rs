//! Wire-level bridge configuration types shared by the cell deploy path (`CellConfig::HttpBridge`/
//! `MqttBridge`) and the myrmic-cli build/nest tooling that produces them.

use crate::MqttConnection;
use serde::{Deserialize, Serialize};

pub use myrmic_common::codegen::bridge_api::{
    WireHttpEndpoint, WireHttpRequestTemplate, WireHttpResponseTemplate, WireHttpResponseVariant,
    WireMqttEgress, WireMqttIngress,
};
pub use myrmic_common::codegen::status::status_variant_name;
pub use myrmic_common::codegen::template::{
    BodyTemplate, ResponseHeaderTemplate, TemplateSegment, TemplateSegments,
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
