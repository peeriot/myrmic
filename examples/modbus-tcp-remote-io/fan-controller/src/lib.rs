#![no_std]

use myrmic_sdk::{self as sdk, Metadata, Result, String, db::state::State};

sdk::import!("../ur20-modbus-tcp-bridge.yml");

const UR20_CLIENT: Ur20Client = Ur20Client::new("ur20");

const FAN_ON_ABOVE_CELSIUS: f32 = 28.0;
const FAN_OFF_BELOW_CELSIUS: f32 = 26.0;

const FAN_STATE: State<bool> = State::new_const("fan");

struct Module {
    name: &'static str,
    id: u32,
}

const RTD_MODULE: Module = Module {
    name: "UR20-4AI-RTD-DIAG",
    id: 0x0406_1544,
};
const DO_MODULE: Module = Module {
    name: "UR20-4DO-P-2A",
    id: 0x0105_2FA0,
};

#[sdk::evt]
fn rtd_module(_md: Metadata, event: RtdModule) -> Result<()> {
    warn_if_unexpected(&RTD_MODULE, event.id);
    Ok(())
}

#[sdk::evt]
fn do_module(_md: Metadata, event: DoModule) -> Result<()> {
    warn_if_unexpected(&DO_MODULE, event.id);
    Ok(())
}

fn warn_if_unexpected(expected: &Module, found: u32) {
    if let Some(warning) = unexpected_module(expected, found) {
        let _ = sdk::warn!("{warning}").ok();
    }
}

fn unexpected_module(expected: &Module, found: u32) -> Option<String> {
    (found != expected.id).then(|| {
        sdk::format!(
            "found module {found:#010x} where {} ({:#010x}) is expected: \
             the addresses in ur20-modbus-tcp-bridge.yml do not fit the coupler",
            expected.name,
            expected.id
        )
    })
}

#[sdk::evt]
fn temperature(_md: Metadata, event: Temperature) -> Result<()> {
    let celsius = f32::from(event.tenths) / 10.0;
    let _ = sdk::debug!("temperature {celsius:.1} °C").ok();
    if let Some(on) = fan_switch(celsius, FAN_STATE.load()?) {
        UR20_CLIENT.fan_on(FanOn { on })?;
        FAN_STATE.save(&on)?;
        let _ = sdk::info!("fan {} at {celsius:.1} °C", if on { "on" } else { "off" }).ok();
    }

    Ok(())
}

fn fan_switch(celsius: f32, fan: Option<bool>) -> Option<bool> {
    let on = if celsius > FAN_ON_ABOVE_CELSIUS {
        true
    } else if celsius < FAN_OFF_BELOW_CELSIUS {
        false
    } else {
        return None;
    };

    (fan != Some(on)).then_some(on)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switches_the_fan_on_above_fan_on() {
        assert_eq!(fan_switch(FAN_ON_ABOVE_CELSIUS, Some(false)), None);
        assert_eq!(
            fan_switch(FAN_ON_ABOVE_CELSIUS + 0.1, Some(false)),
            Some(true)
        );
    }

    #[test]
    fn switches_the_fan_off_below_fan_off() {
        assert_eq!(fan_switch(FAN_OFF_BELOW_CELSIUS, Some(true)), None);
        assert_eq!(
            fan_switch(FAN_OFF_BELOW_CELSIUS - 0.1, Some(true)),
            Some(false)
        );
    }

    #[test]
    fn leaves_the_fan_between_the_thresholds() {
        assert_eq!(fan_switch(27.0, Some(true)), None);
        assert_eq!(fan_switch(27.0, Some(false)), None);
    }

    #[test]
    fn leaves_a_fan_that_already_is_as_it_should_be() {
        assert_eq!(fan_switch(30.0, Some(true)), None);
        assert_eq!(fan_switch(20.0, Some(false)), None);
    }

    #[test]
    fn accepts_the_expected_module() {
        assert_eq!(unexpected_module(&RTD_MODULE, 0x0406_1544), None);
    }

    #[test]
    fn names_the_expected_and_the_found_module() {
        let warning = unexpected_module(&RTD_MODULE, 0x0009_1F84).unwrap();
        assert!(warning.contains("0x00091f84"), "{warning}");
        assert!(
            warning.contains("UR20-4AI-RTD-DIAG (0x04061544)"),
            "{warning}"
        );
    }

    #[test]
    fn switches_a_fan_not_switched_yet() {
        assert_eq!(fan_switch(30.0, None), Some(true));
        assert_eq!(fan_switch(20.0, None), Some(false));
    }
}
