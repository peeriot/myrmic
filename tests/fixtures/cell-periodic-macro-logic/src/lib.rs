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

/// Timer target. Publishes a `timer_tick` event carrying the cell's `uptime()`
/// in milliseconds, so tests can measure the period where it is generated.
#[myrmic_sdk::cmd]
fn tick(_md: Metadata) -> Result<()> {
    let uptime = myrmic_sdk::uptime().map_err(|_| "uptime failed")?;
    let stamp_ms = u64::try_from(uptime.as_millis()).map_err(|_| "uptime overflow")?;
    publish("timer_tick", &JsonValue::from(stamp_ms))?;
    Ok(())
}
