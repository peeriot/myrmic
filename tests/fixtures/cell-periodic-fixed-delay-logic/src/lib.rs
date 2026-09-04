#![no_std]

use core::time::Duration;

use myrmic_sdk::{Callback, JsonValue, Metadata, Result, publish};

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    let _ = myrmic_sdk::interval(Callback::of::<tick>(), Duration::from_millis(50))
        .fixed_delay()
        .build()
        .map_err(|_| "timer failed")?;
    let _ = myrmic_sdk::interval(Callback::of::<tick_b>(), Duration::from_millis(80))
        .fixed_delay()
        .build()
        .map_err(|_| "timer_b failed")?;
    Ok(())
}

/// Slow fixed-delay tick: sleeps 120ms then publishes `fixed_delay_tick`.
#[myrmic_sdk::cmd]
fn tick(_md: Metadata) -> Result<()> {
    myrmic_sdk::info!("fixed_delay tick: entering").ok();
    myrmic_sdk::wait(Duration::from_millis(120)).map_err(|_| "wait failed")?;
    publish("fixed_delay_tick", &JsonValue::from("tick"))?;
    myrmic_sdk::info!("fixed_delay tick: returning").ok();
    Ok(())
}

/// Second fixed-delay tick: sleeps 60ms then publishes `fixed_delay_tick_b`.
#[myrmic_sdk::cmd]
fn tick_b(_md: Metadata) -> Result<()> {
    myrmic_sdk::info!("fixed_delay tick_b: entering").ok();
    myrmic_sdk::wait(Duration::from_millis(60)).map_err(|_| "wait failed")?;
    publish("fixed_delay_tick_b", &JsonValue::from("tick_b"))?;
    myrmic_sdk::info!("fixed_delay tick_b: returning").ok();
    Ok(())
}
