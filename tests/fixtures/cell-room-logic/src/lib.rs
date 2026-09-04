//! Room cell: stores a temperature in the DB and exposes get/set commands.
//! `get_temperature` publishes the current value on the `temperature` event so
//! callers — which can no longer receive a synchronous reply — can observe it.
#![no_std]

use module_examples_common::Temperature;
use myrmic_sdk::db::tree::Kv;
use myrmic_sdk::{Bytes, EventPublishRequest, Metadata, Result, publish_event};

const KV: Kv<Temperature> = Kv::new("room/");
const KEY: &str = "temperature";

#[myrmic_sdk::init]
fn init(_md: Metadata, args: Bytes) -> Result<()> {
    // Init args (if any) seed the starting temperature — the root/CLI
    // counterpart to `spawn_with`. With no args, seed the default only on first
    // init so a redeploy preserves stored state.
    if !args.is_empty() {
        KV.put(KEY, &Temperature::from_payload(&args)?)?;
    } else if KV.get(KEY)?.is_none() {
        KV.put(KEY, &Temperature::new(20))?;
    }
    Ok(())
}

/// Publishes the current room temperature on the `temperature` event.
#[myrmic_sdk::cmd]
fn get_temperature(_md: Metadata) -> Result<()> {
    let temp = KV.get(KEY)?.unwrap_or_else(|| Temperature::new(20));
    publish_event(&EventPublishRequest {
        event: "temperature".try_into()?,
        payload: Some(temp.to_payload()?),
    })?;
    Ok(())
}

/// Sets the room temperature to the given (postcard-encoded) `Temperature`.
#[myrmic_sdk::cmd]
fn set_temperature(_md: Metadata, payload: Bytes) -> Result<()> {
    let temp = Temperature::from_payload(&payload)?;
    KV.put(KEY, &temp)?;
    Ok(())
}
