//! Example cell that sets up a periodic timer in init.
//! The tick function publishes a `timer_tick` event.

#![no_std]

use core::time::Duration;

use myrmic_sdk::{Callback, JsonValue, Metadata, Result, publish};

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    // We never cancel this timer, so dropping the handle is fine — the timer
    // keeps ticking on the host regardless.
    let _handle =
        myrmic_sdk::interval(Callback::of::<tick>(), Duration::from_millis(200)).build()?;
    Ok(())
}

/// The timer target. Timers invoke a `#[cmd]` handler via `Callback`, so this is
/// a command, not a bare export. Publishes a `timer_tick` event on each fire.
#[myrmic_sdk::cmd]
fn tick(_md: Metadata) -> Result<()> {
    publish("timer_tick", &JsonValue::from("tick"))?;
    Ok(())
}
