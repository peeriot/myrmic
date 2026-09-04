#![no_std]

use core::time::Duration;

use myrmic_sdk::{Callback, JsonValue, Metadata, Result, publish};

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    let _ = myrmic_sdk::interval(Callback::of::<tick>(), Duration::from_millis(200))
        .build()
        .map_err(|_| "timer failed")?;
    Ok(())
}

/// Timer target. Publishes a `timer_tick` event on every fire.
#[myrmic_sdk::cmd]
fn tick(_md: Metadata) -> Result<()> {
    publish("timer_tick", &JsonValue::from("tick"))?;
    Ok(())
}
