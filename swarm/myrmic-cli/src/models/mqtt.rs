use sorg_common::{MqttBridge, parse_mqtt_broker_url};

pub use myrmic_common::codegen::bridge_api::{UserMqttBridge, WireMqttEgress, WireMqttIngress};

pub fn convert(cell_name: String, bridge: UserMqttBridge) -> anyhow::Result<MqttBridge> {
    let UserMqttBridge {
        name: _,
        broker_url,
        ingress,
        egress,
    } = bridge;

    parse_mqtt_broker_url(&broker_url)
        .map_err(|err| err.with_validation_context("`broker_url`"))?;

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

#[cfg(test)]
mod tests {
    use sorg_common::parse_mqtt_broker_url;

    use super::{UserMqttBridge, convert};

    #[test]
    fn convert_accepts_and_rejects_what_the_deploy_parser_does() {
        for (url, accepted) in [
            ("mqtt://h", true),
            ("MQTTS://h", true),
            ("mqtts://h:8883", true),
            ("mqtt://[::1]", true),
            ("mqtts://[::1]:8883", true),
            ("mqtt://[fe80::1%3]", true),
            ("mqtts://[fe80::1%3]", false),
            ("mqtt://u@h", false),
            ("mqtt://u:p@h", false),
            ("mqtt://mqtt_broker", false),
            ("mqtt://", false),
            ("mqtt://h/x", false),
            ("mqtt://h?x", false),
            ("http://h", false),
            ("mqtt://h:99999", false),
        ] {
            assert_eq!(convert_url(url).is_ok(), accepted, "convert: {url}");
            assert_eq!(
                parse_mqtt_broker_url(url).is_ok(),
                accepted,
                "shared parser: {url}"
            );
        }
    }

    #[test]
    fn convert_names_the_broker_url_once_in_the_error() {
        let err = convert_url("mqtt://h:0").expect_err("port 0 must be rejected");

        assert_eq!(
            err.to_string(),
            "Validation Error: `broker_url`: broker url port must not be 0"
        );
    }

    fn convert_url(broker_url: &str) -> anyhow::Result<()> {
        convert(
            "cell".to_string(),
            UserMqttBridge {
                name: "bridge".to_string(),
                broker_url: broker_url.to_string(),
                ingress: vec![],
                egress: vec![],
            },
        )
        .map(|_| ())
    }
}
