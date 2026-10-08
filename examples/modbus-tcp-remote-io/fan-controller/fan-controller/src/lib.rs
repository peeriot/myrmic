#![no_std]

use myrmic_sdk::{self as sdk, Metadata, Result, db::state::State};

sdk::import!("../ur20-modbus-tcp-bridge.yml");

const UR20_CLIENT: Ur20Client = Ur20Client::new("ur20");

const FAN_ON_ABOVE_CELSIUS: f32 = 28.0;
const FAN_OFF_BELOW_CELSIUS: f32 = 26.0;

const FAN_STATE: State<bool> = State::new_const("fan");

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
    fn switches_a_fan_not_switched_yet() {
        assert_eq!(fan_switch(30.0, None), Some(true));
        assert_eq!(fan_switch(20.0, None), Some(false));
    }
}
