//! Example cell for the counter/counter-caller pair (caller side).
//!
//! `increment_and_get` fires an `increment` at the counter cell then requests
//! the new value with a callback; `on_count` publishes the reply.

#![no_std]

use myrmic_sdk::{Callback, Metadata, Result, Sri, publish, send};

const COUNTER_SRI: &str = "counter_cell";

#[myrmic_sdk::cmd]
fn increment_and_get(_md: Metadata, value: i32) -> Result<()> {
    let counter = Sri::of_path(COUNTER_SRI).map_err(|_| "invalid sri")?;
    send(counter, "increment", &value)?;
    send(counter, "get_count", &Callback::of::<on_count>())?;
    Ok(())
}

#[myrmic_sdk::cmd]
fn on_count(_md: Metadata, cv: i32) -> Result<()> {
    publish("counter_value", &cv)?;
    Ok(())
}
