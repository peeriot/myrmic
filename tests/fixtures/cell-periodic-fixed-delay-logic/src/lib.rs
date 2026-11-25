#![no_std]

use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;

use myrmic_sdk::{Callback, JsonValue, Metadata, Result, publish, vec};

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    let _ = myrmic_sdk::interval(Callback::of::<tick>(), Duration::from_millis(200))
        .fixed_delay()
        .build()
        .map_err(|_| "timer failed")?;
    let _ = myrmic_sdk::interval(Callback::of::<tick_b>(), Duration::from_millis(80))
        .fixed_delay()
        .build()
        .map_err(|_| "timer_b failed")?;
    Ok(())
}

/// `uptime()` stamp taken right before the previous tick returned, 0 before the first tick.
static LAST_EXIT_US: AtomicU64 = AtomicU64::new(0);

/// Slow fixed-delay tick: sleeps 300ms, then publishes `fixed_delay_tick` with
/// `[entry_us, prev_exit_us]`, the `uptime()` stamps of this handler's entry and of the
/// previous handler's exit. The exit stamp is the last statement before returning, so the
/// handler's own publish and logging are inside the measured window.
#[myrmic_sdk::cmd]
fn tick(_md: Metadata) -> Result<()> {
    let entry_us = uptime_us()?;
    myrmic_sdk::info!("fixed_delay tick: entering").ok();
    myrmic_sdk::wait(Duration::from_millis(300)).map_err(|_| "wait failed")?;
    publish(
        "fixed_delay_tick",
        &JsonValue::from(vec![entry_us, LAST_EXIT_US.load(Ordering::Relaxed)]),
    )?;
    myrmic_sdk::info!("fixed_delay tick: returning").ok();
    LAST_EXIT_US.store(uptime_us()?, Ordering::Relaxed);

    Ok(())
}

/// Second fixed-delay tick: sleeps 60ms, then publishes `fixed_delay_tick_b`.
///
/// No test in this repository uses it, but it isn't unused: the HIL tests in swarm-ci run this
/// fixture on a board (`fixed_delay_two_timers_both_fire`) and check that two fixed-delay timers
/// both keep firing. It can only delay `tick`'s next entry, never bring it forward, so it doesn't
/// disturb the lower bound that sorg-execution's fixed-delay test checks.
#[myrmic_sdk::cmd]
fn tick_b(_md: Metadata) -> Result<()> {
    myrmic_sdk::info!("fixed_delay tick_b: entering").ok();
    myrmic_sdk::wait(Duration::from_millis(60)).map_err(|_| "wait failed")?;
    publish("fixed_delay_tick_b", &JsonValue::from("tick_b"))?;
    myrmic_sdk::info!("fixed_delay tick_b: returning").ok();
    Ok(())
}

fn uptime_us() -> Result<u64> {
    let uptime = myrmic_sdk::uptime().map_err(|_| "uptime failed")?;

    u64::try_from(uptime.as_micros()).map_err(|_| "uptime overflow")
}
