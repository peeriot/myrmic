//! A minimal counter cell: `increment` bumps a stored count and publishes
//! the new value as an event.

#![no_std]

use myrmic_sdk::db::tree::Kv;
use myrmic_sdk::{Metadata, Result, publish};

/// Published when the counter value changes.
#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
struct CountChanged {
    count: i32,
}

const KV: Kv<i32> = Kv::new("counter");

/// Runs once per incarnation, before any other handler.
#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    if KV.get("count")?.is_none() {
        KV.put("count", &0)?;
    }
    Ok(())
}

/// Invocable by other cells (or the gateway) as the `increment` command.
#[myrmic_sdk::cmd]
fn increment(_md: Metadata, by: i32) -> Result<()> {
    let n = KV.get("count")?.unwrap_or(0) + by;
    KV.put("count", &n)?;
    publish("count_changed", &CountChanged { count: n })
}
