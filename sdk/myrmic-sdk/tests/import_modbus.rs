//! Compiles the client `import!` generates for a Modbus bridge, and pins its
//! types to the JSON the Modbus bridge in `sorg-execution` sends and expects.

use myrmic_sdk::{Decoder, Encoder};

myrmic_sdk::import!("tests/data/modbus-bridge.yml");

#[test]
#[expect(
    clippy::float_cmp,
    reason = "21.5 is exact in f32, and nothing computes it"
)]
fn a_polled_value_decodes_from_the_event_the_bridge_publishes() {
    let event = BoilerTemperature::from_bytes(br#"{"celsius": 21.5}"#.to_vec()).unwrap();
    assert_eq!(event.celsius, 21.5);
}

#[test]
fn a_written_value_encodes_as_the_command_the_bridge_expects() {
    let bytes = PumpOn { on: true }.to_bytes().unwrap();
    assert_eq!(bytes, br#"{"on":true}"#);
}

#[test]
fn a_read_reply_decodes_from_each_answer_the_bridge_sends() {
    let ok = ReadSetpointReply::from_bytes(br#"{"Ok": {"setpoint": -5}}"#.to_vec()).unwrap();
    assert!(matches!(
        ok,
        ReadSetpointReply::Ok(ReadSetpointValue { setpoint: -5 })
    ));

    let exception = ReadSetpointReply::from_bytes(br#"{"Exception": 2}"#.to_vec()).unwrap();
    assert!(matches!(exception, ReadSetpointReply::Exception(2)));

    let failed = ReadSetpointReply::from_bytes(br#"{"Failed": "timeout"}"#.to_vec()).unwrap();
    assert!(matches!(failed, ReadSetpointReply::Failed(reason) if reason == "timeout"));
}

#[test]
fn the_client_binds_to_a_bridge_target() {
    const _BOILER: BoilerClient = BoilerClient::new("plant/boiler");
}
