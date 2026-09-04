//! Per-cell counter with an artificial processing delay.
//!
//! `process` takes a u32 delay (ms), waits for it, publishes `processed` with
//! `"{sri}:{count}"` (pre-increment), then bumps the per-cell counter. The delay
//! keeps a cell busy so the queuing/parallelism timing assertions hold.
#![no_std]

use core::time::Duration;

use myrmic_sdk::db::tree::Kv;
use myrmic_sdk::{Bytes, EventPublishRequest, Metadata, Result, format, publish_event, wait};

const COUNTER: Kv<u32> = Kv::new("counter/");
const KEY: &str = "count";

#[myrmic_sdk::cmd]
fn process(md: Metadata, payload: Bytes) -> Result<()> {
    let delay_ms: u32 = postcard::from_bytes(&payload).map_err(|_| "bad delay payload")?;

    let count = COUNTER.get(KEY)?.unwrap_or(0);
    wait(Duration::from_millis(u64::from(delay_ms)))?;
    publish_event(&EventPublishRequest {
        event: "processed".try_into()?,
        payload: Some(format!("{}:{}", md.id, count).into_bytes()),
    })?;
    COUNTER.put(KEY, &(count + 1))?;
    Ok(())
}
