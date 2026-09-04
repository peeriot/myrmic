//! Main cell for the sequential-processing test.
//!
//! `call_helper` fires a delay off to the helper cell, then increments a
//! per-cell counter. `read_counter` publishes `counter_read` with
//! `"{sri}:{count}"`.
//!
//! NOTE: the original `call_helper` used a *synchronous* cross-cell call
//! (`command::<Synchronous>::send_wait`), which no longer exists. It is now a
//! fire-and-forget `send`, so this cell no longer blocks on the helper;
//! the test remains a per-cell FIFO-ordering check only.
#![no_std]

use myrmic_sdk::db::tree::Kv;
use myrmic_sdk::{EventPublishRequest, Metadata, Result, Sri, format, publish_event, send};

const HELPER_SRI: &str = "helper_cell";
const COUNTER: Kv<u32> = Kv::new("seq/");
const KEY: &str = "count";

#[myrmic_sdk::cmd]
fn call_helper(_md: Metadata, delay_ms: u32) -> Result<()> {
    let helper = Sri::of_path(HELPER_SRI).map_err(|_| "invalid helper sri")?;
    send(helper, "slow_op", &delay_ms)?;

    let count = COUNTER.get(KEY)?.unwrap_or(0);
    COUNTER.put(KEY, &(count + 1))?;
    Ok(())
}

#[myrmic_sdk::cmd]
fn read_counter(md: Metadata) -> Result<()> {
    let count = COUNTER.get(KEY)?.unwrap_or(0);
    publish_event(&EventPublishRequest {
        event: "counter_read".try_into()?,
        payload: Some(format!("{}:{}", md.id, count).into_bytes()),
    })?;
    Ok(())
}
