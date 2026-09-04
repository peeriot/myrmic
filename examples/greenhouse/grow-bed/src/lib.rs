//! Grow-bed asset: owns the canonical state of one bed of plants - the latest
//! moisture reading, the pump status, and the moisture range the plants want.
//! Every change is announced on the `bed_state` event. It commands nothing:
//! actuation belongs to the pump adapter, decisions to the irrigation agent.
//!
//! Part 3 of the Smart Greenhouse tutorial.
#![no_std]

use myrmic_sdk::db::state::State;
use myrmic_sdk::{Metadata, Result, publish};

/// Canonical state of the bed - also the payload of the `bed_state` event.
#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
struct Bed {
    moisture: f32,
    pump_on: bool,
    target_low: f32,
    target_high: f32,
}

impl Default for Bed {
    fn default() -> Self {
        Self {
            moisture: 0.0,
            pump_on: false,
            target_low: 55.0,
            target_high: 75.0,
        }
    }
}

const BED: State<Bed> = State::new_const("bed");

/// The moisture range the plants in this bed want, settable at runtime.
#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
struct TargetRange {
    low: f32,
    high: f32,
}

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    let bed = BED.load()?.unwrap_or_default();

    BED.save(&bed)
}

/// A new sensor reading: update the canonical state and announce it.
#[myrmic_sdk::evt]
fn moisture(_md: Metadata, value: f32) -> Result<()> {
    let mut bed = BED.load()?.unwrap_or_default();
    bed.moisture = value;
    BED.save(&bed)?;

    publish("bed_state", &bed)
}

/// The pump announced a state change: record and announce it.
#[myrmic_sdk::evt]
fn pump_state(_md: Metadata, on: bool) -> Result<()> {
    let mut bed = BED.load()?.unwrap_or_default();
    bed.pump_on = on;
    BED.save(&bed)?;

    publish("bed_state", &bed)
}

/// Domain command: this bed now grows plants that want a different range.
#[myrmic_sdk::cmd]
fn set_target(_md: Metadata, range: TargetRange) -> Result<()> {
    let mut bed = BED.load()?.unwrap_or_default();
    bed.target_low = range.low;
    bed.target_high = range.high;
    BED.save(&bed)?;

    publish("bed_state", &bed)
}
