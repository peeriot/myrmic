//! Mock soil-moisture sensor: publishes a `moisture` reading every second.
//! The simulated moisture swings between a dry and a wet bound, the way real
//! soil follows the weather.
//!
//! Part 1 of the Smart Greenhouse tutorial.
#![no_std]

use core::time::Duration;

use myrmic_sdk::db::state::State;
use myrmic_sdk::{Callback, Metadata, Result, publish};

/// Simulated soil moisture, in percent.
const MOISTURE: State<f32> = State::new_const("moisture");

/// Direction of the swing: `true` while the simulated weather is wetting.
const RISING: State<bool> = State::new_const("rising");

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    MOISTURE.save(&65.0)?;
    // The handle could cancel the timer later; this sensor measures forever.
    let _ = myrmic_sdk::interval(Callback::of::<measure>(), Duration::from_secs(1))
        .build()
        .map_err(|_| "timer failed")?;

    Ok(())
}

/// Timer target: the simulation advances one step, then the reading is
/// published. The value swings: it dries down to 35%, turns, wets up to 90%.
#[myrmic_sdk::cmd]
fn measure(_md: Metadata) -> Result<()> {
    let mut moisture = MOISTURE.load()?.unwrap_or(65.0);
    let mut rising = RISING.load()?.unwrap_or_default();

    if moisture <= 35.0 {
        rising = true;
    } else if moisture >= 90.0 {
        rising = false;
    }

    let delta = if rising { 1.2 } else { -0.4 };
    moisture = (moisture + delta).clamp(0.0, 100.0);

    MOISTURE.save(&moisture)?;
    RISING.save(&rising)?;

    publish("moisture", &moisture)
}

/// A rain shower adds a one-time amount of moisture.
#[myrmic_sdk::evt]
fn rain(_md: Metadata, amount: f32) -> Result<()> {
    let mut moisture = MOISTURE.load()?.unwrap_or(65.0);
    moisture = (moisture + amount).clamp(0.0, 100.0);

    MOISTURE.save(&moisture)
}
