//! Pump adapter: `start` and `stop` drive the irrigation motor, and the
//! current state is announced on the `pump_state` event. The pump knows
//! nothing about plants or watering policy.
//!
//! Part 2 of the Smart Greenhouse tutorial.
#![no_std]

use myrmic_sdk::db::state::State;
use myrmic_sdk::{Metadata, Result, publish};

const RUNNING: State<bool> = State::new_const("running");

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    RUNNING.save(&false)?;

    publish("pump_state", &false)
}

#[myrmic_sdk::cmd]
fn start(md: Metadata) -> Result<()> {
    RUNNING.save(&true)?;
    let _ = myrmic_sdk::info!("pump started (sender={:?})", md.sender).ok();

    publish("pump_state", &true)
}

#[myrmic_sdk::cmd]
fn stop(md: Metadata) -> Result<()> {
    RUNNING.save(&false)?;
    let _ = myrmic_sdk::info!("pump stopped (sender={:?})", md.sender).ok();

    publish("pump_state", &false)
}
