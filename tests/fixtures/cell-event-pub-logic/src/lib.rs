//! Example cell for testing event publishing.
//!
//! Stores a counter in the DB. Each `trigger` command increments it and
//! publishes the new value on the `count_changed` event.

#![no_std]

use myrmic_sdk::db::tree::Kv;
use myrmic_sdk::{Metadata, Result, publish};

/// A simple counter value.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy)]
struct Counter {
    count: i32,
}

/// Published when the counter value changes.
#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
#[codec(myrmic_sdk::Postcard)]
struct CountChanged {
    counter: Counter,
}

const KV: Kv<Counter> = Kv::new("event_pub/");
const KEY: &str = "counter";

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    if KV.get(KEY)?.is_none() {
        KV.put(KEY, &Counter { count: 0 })?;
    }
    Ok(())
}

#[myrmic_sdk::cmd]
fn trigger(_md: Metadata) -> Result<()> {
    let mut c = KV.get(KEY)?.unwrap_or(Counter { count: 0 });
    c.count += 1;
    KV.put(KEY, &c)?;
    publish("count_changed", &CountChanged { counter: c })?;
    Ok(())
}
