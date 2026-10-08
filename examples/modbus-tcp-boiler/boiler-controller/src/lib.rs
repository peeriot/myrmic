#![no_std]

use myrmic_sdk::{self as sdk, Callback, Metadata, Result, db::state::State};
use serde::{Deserialize, Serialize};

sdk::import!("../boiler-modbus-tcp-bridge.yml");

const BOILER_CLIENT: BoilerClient = BoilerClient::new("boiler");

const HYSTERESIS_CELSIUS: f32 = 2.0;

const SETPOINT_STATE: State<i16> = State::new_const("setpoint");
const REPORTED_BURNING_STATE: State<bool> = State::new_const("burning");

#[derive(Serialize, Deserialize, sdk::Message)]
struct Setpoint {
    setpoint: i16,
}

#[sdk::evt]
fn boiler_temperature(_md: Metadata, event: BoilerTemperature) -> Result<()> {
    let Some(setpoint) = SETPOINT_STATE.load()? else {
        return BOILER_CLIENT.read_setpoint(Callback::of::<setpoint_read>());
    };

    let reported_burning = REPORTED_BURNING_STATE.load()?;
    if let Some(burn) = burner_switch(event.celsius, setpoint, reported_burning) {
        BOILER_CLIENT.burner_on(BurnerOn { on: burn })?;
        let _ = sdk::info!(
            "burner {} at {:.1} °C",
            if burn { "on" } else { "off" },
            event.celsius
        )
        .ok();
    }

    Ok(())
}

fn burner_switch(celsius: f32, setpoint: i16, reported_burning: Option<bool>) -> Option<bool> {
    let setpoint = f32::from(setpoint);
    let burn = if celsius < setpoint - HYSTERESIS_CELSIUS {
        true
    } else if celsius > setpoint + HYSTERESIS_CELSIUS {
        false
    } else {
        return None;
    };

    (reported_burning != Some(burn)).then_some(burn)
}

#[sdk::evt]
fn burner(_md: Metadata, event: Burner) -> Result<()> {
    REPORTED_BURNING_STATE.save(&event.on)
}

#[sdk::cmd]
fn set_setpoint(_md: Metadata, new: Setpoint) -> Result<()> {
    BOILER_CLIENT.set_setpoint(SetSetpoint {
        setpoint: new.setpoint,
    })?;
    SETPOINT_STATE.save(&new.setpoint)?;
    let _ = sdk::info!("setpoint {} °C", new.setpoint).ok();

    Ok(())
}

#[sdk::cmd]
fn setpoint_read(_md: Metadata, reply: ReadSetpointReply) -> Result<()> {
    match reply {
        ReadSetpointReply::Ok(value) => {
            let setpoint_set_meanwhile = SETPOINT_STATE.load()?.is_some();
            if !setpoint_set_meanwhile {
                SETPOINT_STATE.save(&value.setpoint)?;
                let _ = sdk::info!("setpoint {} °C", value.setpoint).ok();
            }
        }
        ReadSetpointReply::Exception(code) => {
            let _ = sdk::warn!("boiler refused to read the setpoint: {code}").ok();
        }
        ReadSetpointReply::Failed(reason) => {
            let _ = sdk::warn!("boiler did not answer: {reason}").ok();
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switches_on_below_the_hysteresis() {
        assert_eq!(burner_switch(57.9, 60, Some(false)), Some(true));
    }

    #[test]
    fn switches_off_above_the_hysteresis() {
        assert_eq!(burner_switch(62.1, 60, Some(true)), Some(false));
    }

    #[test]
    fn leaves_the_burner_within_the_hysteresis() {
        assert_eq!(burner_switch(61.0, 60, Some(true)), None);
        assert_eq!(burner_switch(59.0, 60, Some(false)), None);
    }

    #[test]
    fn leaves_a_burner_that_already_is_as_it_should_be() {
        assert_eq!(burner_switch(50.0, 60, Some(true)), None);
    }

    #[test]
    fn switches_a_burner_not_reported_yet() {
        assert_eq!(burner_switch(50.0, 60, None), Some(true));
        assert_eq!(burner_switch(70.0, 60, None), Some(false));
    }
}
