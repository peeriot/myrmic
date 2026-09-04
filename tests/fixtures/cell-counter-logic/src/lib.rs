//! Example cell for the counter/counter-caller pair (callee side).
//!
//! Stores an `i32` count in the DB. `increment` bumps it; `get_count` replies
//! to the caller via the supplied callback.

#![no_std]

use myrmic_sdk::db::tree::Kv;
use myrmic_sdk::{Callback, Metadata, Result};

const KV: Kv<i32> = Kv::new("counter/");
const KEY: &str = "count";

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    if KV.get(KEY)?.is_none() {
        KV.put(KEY, &0)?;
    }
    Ok(())
}

#[myrmic_sdk::cmd]
fn increment(_md: Metadata, incr: i32) -> Result<()> {
    let n = KV.get(KEY)?.unwrap_or(0);
    KV.put(KEY, &(n + incr))?;
    Ok(())
}

#[myrmic_sdk::cmd]
fn get_count(md: Metadata, cb: Callback<i32>) -> Result<()> {
    let n = KV.get(KEY)?.unwrap_or(0);
    cb.invoke(md.sender, &n)?;
    Ok(())
}
