#![no_std]

use myrmic_sdk::{Callback, Metadata, Result};

myrmic_sdk::import!("../bridge_http.yml", "../bridge_mqtt.yml");

#[myrmic_sdk::init]
fn init(_: Metadata) -> Result<()> {
    Ok(())
}

#[myrmic_sdk::cmd]
fn test_http(_: Metadata) -> Result<()> {
    HttpClient::new("bridge.http").test(Callback::of::<on_test_http>())
}

#[myrmic_sdk::cmd]
fn on_test_http(_: Metadata, reply: TestReply) -> Result<()> {
    match reply {
        TestReply::Ok(response) => {
            MqttClient::new("bridge.mqtt").publish_http_response(PublishHttpResponse {
                data: myrmic_sdk::JsonValue::String(response.body),
            })
        }
        TestReply::Unknown(status) => Err(match status {
            _ => "unexpected HTTP response status",
        }),
    }
}

#[myrmic_sdk::evt]
fn receive_request(_: Metadata, event: ReceiveRequest) -> Result<()> {
    MqttClient::new("bridge.mqtt").publish_response(PublishResponse { data: event.data })
}
