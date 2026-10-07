//! Boiler controller: keeps the water of the simulated boiler around its
//! setpoint by switching the burner, all through the Modbus bridge.
//!
//! - the water temperature arrives as the polled `boiler_temperature` event,
//! - the setpoint is read from the boiler once, when the first reading arrives,
//! - the setpoint is changed through this cell, with `set_setpoint`: it writes
//!   the new value to the boiler and regulates to it from then on,
//! - the burner is switched by writing its coil, and its state arrives as the
//!   polled `burner` event whenever it changes.
#![no_std]

use myrmic_sdk::{Callback, Metadata, Result, db::state::State};

myrmic_sdk::import!("../boiler-modbus-tcp-bridge.yml");

/// The bridge, by the name the application deploys it under.
const BOILER_CLIENT: BoilerClient = BoilerClient::new("boiler");

/// How far the water may drift from the setpoint before the burner switches:
/// without it, the burner would flicker on and off at the setpoint.
const HYSTERESIS: f32 = 2.0;

/// The setpoint the controller regulates to; `None` until it is known.
const SETPOINT_STATE: State<Option<i16>> = State::new_const("setpoint");
/// Whether the burner burns, as the boiler last reported it; unset until it has.
const BURNING_STATE: State<bool> = State::new_const("burning");

/// A new setpoint, in °C.
#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
struct Setpoint {
    setpoint: i16,
}

#[myrmic_sdk::evt]
fn boiler_temperature(_md: Metadata, event: BoilerTemperature) -> Result<()> {
    let Some(setpoint) = SETPOINT_STATE.load()?.flatten() else {
        // Not known yet: ask, and decide on a later reading.
        return BOILER_CLIENT.read_setpoint(Callback::of::<setpoint_read>());
    };

    let setpoint = f32::from(setpoint);

    let burn = if event.celsius < setpoint - HYSTERESIS {
        true
    } else if event.celsius > setpoint + HYSTERESIS {
        false
    } else {
        // Within the hysteresis the burner stays as it is.
        return Ok(());
    };

    // Not saved here: the `burner` event reports what the boiler made of the
    // write. Until it does, a later reading may switch the burner once more.
    if BURNING_STATE.load()? != Some(burn) {
        BOILER_CLIENT.burner_on(BurnerOn { on: burn })?;
        let _ = myrmic_sdk::info!(
            "burner {} at {:.1} °C",
            if burn { "on" } else { "off" },
            event.celsius
        )
        .ok();
    }

    Ok(())
}

/// Takes the burner's state from the boiler, so that it cannot drift from what
/// the controller believes: a write may fail, or someone else may switch it.
#[myrmic_sdk::evt]
fn burner(_md: Metadata, event: Burner) -> Result<()> {
    BURNING_STATE.save(&event.on)
}

/// Changes the setpoint: writes it to the boiler, and regulates to it from the
/// next reading on. Going through the controller, rather than writing to the
/// boiler directly, keeps the two in step.
#[myrmic_sdk::cmd]
fn set_setpoint(_md: Metadata, new: Setpoint) -> Result<()> {
    BOILER_CLIENT.set_setpoint(SetSetpoint {
        setpoint: new.setpoint,
    })?;
    SETPOINT_STATE.save(&Some(new.setpoint))?;
    let _ = myrmic_sdk::info!("setpoint {} °C", new.setpoint).ok();

    Ok(())
}

#[myrmic_sdk::cmd]
fn setpoint_read(_md: Metadata, reply: ReadSetpointReply) -> Result<()> {
    match reply {
        ReadSetpointReply::Ok(value) => {
            // A setpoint set in the meantime is newer than the one read.
            if SETPOINT_STATE.load()?.flatten().is_none() {
                SETPOINT_STATE.save(&Some(value.setpoint))?;
                let _ = myrmic_sdk::info!("setpoint {} °C", value.setpoint).ok();
            }
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
