//! Child cell spawned by `cell-spawner`.
//!
//! Seeds a default value in the DB on `#[init]`. Commands are fire-and-forget, so
//! `get_value` publishes the stored value on the `child_value` event instead of
//! returning it; the host subscribes to observe it.
#![no_std]

use myrmic_sdk::db::tree::Kv;
use myrmic_sdk::{Codec, EventPublishRequest, Metadata, Postcard, Result, publish_event};

const KV: Kv<i32> = Kv::new("spawn_child/");
const KEY: &str = "value";
const VALUE_EVENT: &str = "child_value";

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    // Seed the default only on first init; a redeploy preserves stored state.
    if KV.get(KEY)?.is_none() {
        KV.put(KEY, &0)?;
    }
    Ok(())
}

/// Publishes the current value on the `child_value` event.
#[myrmic_sdk::cmd]
fn get_value(_md: Metadata) -> Result<()> {
    let value = KV.get(KEY)?.unwrap_or(0);
    publish_event(&EventPublishRequest {
        event: VALUE_EVENT.try_into()?,
        payload: Some(Postcard::encode(&value)?),
    })?;
    Ok(())
}
