#![no_std]

extern crate alloc;

use alloc::string::ToString;
use wasm_sdk::{
    String,
    macros::{cell, import},
};

import!("../bridge_http.yml");
import!("../bridge_mqtt.yml");

wasm_sdk::cell_prelude!();

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct TestCell {}

#[cell]
impl TestCell {
    #[init]
    fn init() -> Self {
        Self {}
    }

    #[command]
    fn test_http(&mut self) -> wasm_sdk::Result<String> {
        let http_client = HttpClient::new("bridge.http");

        let Ok(response) = http_client.test() else {
            return Err("http request failed");
        };

        Ok(response.body.to_string())
    }

    #[event_handler]
    fn receive_request(&mut self, event: ReceiveRequest) {
        let mqtt_client = MqttClient::new("bridge.mqtt");
        let _ = mqtt_client.publish_response(PublishResponse { data: event.data });
    }
}
