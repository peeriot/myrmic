use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

pub use crate::codegen::cell_api::ApiType;
pub use crate::codegen::template::{
    BodyTemplate, ParseInto, ResponseHeaderTemplate, TemplateSegments,
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
}
