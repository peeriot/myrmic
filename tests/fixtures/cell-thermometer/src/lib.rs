//! Thermometer cell: a periodic timer publishes a simulated temperature on the
//! `temperature_celsius` event while active. `start`/`stop` toggle publishing.
#![no_std]

use core::time::Duration;

use myrmic_sdk::db::tree::Kv;
use myrmic_sdk::{Callback, Metadata, Result, publish};

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct ThermometerState {
    active: bool,
    last_temp: f32,
}

const KV: Kv<ThermometerState> = Kv::new("thermometer/");
const KEY: &str = "state";

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    if KV.get(KEY)?.is_none() {
        KV.put(KEY, &ThermometerState::default())?;
    }
    myrmic_sdk::interval(Callback::of::<measure>(), Duration::from_secs(5))
        .build()
        .map_err(|_| "timer failed")?;
    Ok(())
}

#[myrmic_sdk::cmd]
fn start(_md: Metadata) -> Result<()> {
    let mut state = KV.get(KEY)?.unwrap_or_default();
    state.active = true;
    KV.put(KEY, &state)?;
    Ok(())
}

#[myrmic_sdk::cmd]
fn stop(_md: Metadata) -> Result<()> {
    let mut state = KV.get(KEY)?.unwrap_or_default();
    state.active = false;
    KV.put(KEY, &state)?;
    Ok(())
}

/// Timer target: publishes a simulated temperature while active.
#[myrmic_sdk::cmd]
fn measure(_md: Metadata) -> Result<()> {
    let mut state = KV.get(KEY)?.unwrap_or_default();
    if state.active {
        let celsius = state.last_temp % 25.0 + 11.0;
        publish("temperature_celsius", &celsius)?;
        state.last_temp += 0.1;
        KV.put(KEY, &state)?;
    }
    Ok(())
}
