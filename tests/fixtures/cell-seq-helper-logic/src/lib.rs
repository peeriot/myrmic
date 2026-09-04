//! Helper cell for the sequential-processing test: `slow_op` waits for the
//! requested delay. Invoked fire-and-forget by the main cell.
#![no_std]

use core::time::Duration;

use myrmic_sdk::{Metadata, Result, wait};

#[myrmic_sdk::cmd]
fn slow_op(_md: Metadata, delay_ms: u32) -> Result<()> {
    wait(Duration::from_millis(u64::from(delay_ms)))?;
    Ok(())
}
