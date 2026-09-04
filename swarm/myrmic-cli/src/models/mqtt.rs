use myrmic_common::types::web::{Scheme, Url};
use sorg_common::MqttBridge;

pub use myrmic_common::codegen::bridge_api::{UserMqttBridge, WireMqttEgress, WireMqttIngress};

pub fn convert(cell_name: String, bridge: UserMqttBridge) -> anyhow::Result<MqttBridge> {
    let UserMqttBridge {
        name: _,
        broker_url,
        ingress,
        egress,
    } = bridge;

    let url = Url::parse(&broker_url)
        .map_err(|err| anyhow::anyhow!("unable to parse `broker_url`: {}", err.to_text()))?;

    if !matches!(url.scheme(), Scheme::Mqtt | Scheme::Mqtts) {
        anyhow::bail!("`broker_url` only supports mqtt(s)")
    }

    let ingress = ingress
        .into_iter()
        .map(|v| WireMqttIngress {
            id: v.id,
            topic: v.topic,
            qos: v.qos,
            payload: v.payload.0,
        })
        .collect();

    let egress = egress
        .into_iter()
        .map(|v| WireMqttEgress {
            id: v.id,
            topic: v.topic.0,
            qos: v.qos,
            payload: v.payload.0,
        })
        .collect();

    Ok(MqttBridge {
        cell_name,
        broker: broker_url,
        egress,
        ingress,
    })
}
