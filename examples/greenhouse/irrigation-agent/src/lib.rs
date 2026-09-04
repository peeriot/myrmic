//! Irrigation agent: the only cell that makes decisions. It reads the
//! grow-bed's canonical state - never the raw sensor - and drives the pump
//! adapter with hysteresis: start below the bed's low target, stop above the
//! high one, so the pump never chatters around a single set point.
//!
//! Part 4 of the Smart Greenhouse tutorial.
#![no_std]

use myrmic_sdk::db::state::State;
use myrmic_sdk::{Metadata, Result, Sri, Void, publish, send};

const PUMP: &str = "pump";

/// Hysteresis flag: are we in the middle of a watering cycle?
const WATERING: State<bool> = State::new_const("watering");

/// Payload of the grow-bed's `bed_state` event. Declared here as well as in the
/// grow-bed: cells share wire formats, not Rust types.
#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
struct BedState {
    moisture: f32,
    pump_on: bool,
    target_low: f32,
    target_high: f32,
}

#[myrmic_sdk::evt]
fn bed_state(_md: Metadata, bed: BedState) -> Result<()> {
    let watering = WATERING.load()?.unwrap_or_default();

    if !watering && bed.moisture < bed.target_low {
        pump("start")?;
        WATERING.save(&true)?;
        publish("watering_started", &bed.moisture)?;
    } else if watering && bed.moisture > bed.target_high {
        pump("stop")?;
        WATERING.save(&false)?;
        publish("watering_stopped", &bed.moisture)?;
    }

    Ok(())
}

fn pump(command: &str) -> Result<()> {
    let pump = Sri::of_path(PUMP).map_err(|_| "invalid pump srn")?;

    send(pump, command, &Void)
}
