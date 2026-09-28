#![no_std]

use myrmic_sdk::{Callback, Metadata, Result, db::state::State};

myrmic_sdk::import!("../boiler_modbus_tcp_bridge.yml");

const BOILER_CLIENT: BoilerClient = BoilerClient::new("boiler");

const SETPOINT: State<Option<i16>> = State::new_const("setpoint");
const BURNING: State<bool> = State::new_const("burning");

/// How far the water may drift from the setpoint before the burner switches:
/// without it, the burner would flicker on and off at the setpoint.
const HYSTERESIS: f32 = 2.0;

#[myrmic_sdk::init]
fn init(md: Metadata) -> myrmic_sdk::Result {
    let _ = myrmic_sdk::info!("starting boiler controller cell (id={:?})", md.id).ok();
    Ok(())
}

#[myrmic_sdk::evt]
fn boiler_temperature(_md: Metadata, event: BoilerTemperature) -> Result<()> {
    let Some(setpoint) = SETPOINT.load()?.flatten() else {
        // Not known yet: ask, and decide on a later reading.
        return BOILER_CLIENT.read_setpoint(Callback::of::<setpoint_read>());
    };

    let setpoint = f32::from(setpoint);
    let burning = BURNING.load()?.unwrap_or_default();

    let burn = if event.celsius < setpoint - HYSTERESIS {
        true
    } else if event.celsius > setpoint + HYSTERESIS {
        false
    } else {
        burning
    };

    if burn != burning {
        BOILER_CLIENT.burner_on(BurnerOn { on: burn })?;
        BURNING.save(&burn)?;
        let _ = myrmic_sdk::info!(
            "burner {} at {:.1} °C",
            if burn { "on" } else { "off" },
            event.celsius
        )
        .ok();
    }

    Ok(())
}

#[myrmic_sdk::cmd]
fn setpoint_read(_md: Metadata, reply: ReadSetpointReply) -> Result<()> {
    match reply {
        ReadSetpointReply::Ok(value) => {
            SETPOINT.save(&Some(value.setpoint))?;
            let _ = myrmic_sdk::info!("setpoint {} °C", value.setpoint).ok();
        }
        ReadSetpointReply::Exception(code) => {
            let _ = myrmic_sdk::warn!("boiler refused to read the setpoint: {code}").ok();
        }
        ReadSetpointReply::Failed(reason) => {
            let _ = myrmic_sdk::warn!("boiler did not answer: {reason}").ok();
        }
    }

    Ok(())
}
