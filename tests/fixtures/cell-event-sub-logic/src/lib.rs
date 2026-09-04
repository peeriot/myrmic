//! Example cell for testing event subscription.
//!
//! Subscribes to the `count_changed` event from the publisher cell (the runtime
//! auto-subscribes to the `event_count_changed` export). On receipt, forwards
//! the counter value by publishing a `count_forwarded` event.

#![no_std]

use myrmic_sdk::{Metadata, Result, publish};

/// A simple counter value.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy)]
struct Counter {
    count: i32,
}

/// Received from the publisher cell.
#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
#[codec(myrmic_sdk::Postcard)]
struct CountChanged {
    counter: Counter,
}

#[myrmic_sdk::evt]
fn count_changed(_md: Metadata, ev: CountChanged) -> Result<()> {
    publish("count_forwarded", &ev.counter.count)?;
    Ok(())
}
