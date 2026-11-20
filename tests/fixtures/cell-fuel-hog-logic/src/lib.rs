//! Limits cell: `hog` never returns, `ping` answers on the `hog_ping` event,
//! `grow` publishes the result of a `memory.grow` on the `hog_grow` event.
//!
//! Fixture for the cell limits: a runaway handler must be ended by the
//! runtime's fuel budget while neighbouring cells keep answering, and memory
//! growth past the memory limit must fail instead of ending the cell.

#![no_std]

use myrmic_sdk::{Metadata, Result, publish};

/// Spins forever. Only the runtime's fuel budget can end this run.
#[myrmic_sdk::cmd]
fn hog(_md: Metadata) -> Result<()> {
    loop {
        core::hint::spin_loop();
    }
}

/// Publishes `tag` on the `hog_ping` event.
#[myrmic_sdk::cmd]
fn ping(_md: Metadata, tag: i32) -> Result<()> {
    publish("hog_ping", &tag)?;
    Ok(())
}

/// Grows the memory by `pages` and publishes the old size in pages on the
/// `hog_grow` event, or `-1` when the growth was refused.
#[cfg(target_arch = "wasm32")]
#[myrmic_sdk::cmd]
fn grow(_md: Metadata, pages: u32) -> Result<()> {
    // A refusal is reported as `usize::MAX`, which does not fit an `i32` and
    // becomes `-1` below.
    let old = core::arch::wasm32::memory_grow(0, pages as usize);
    publish("hog_grow", &i32::try_from(old).unwrap_or(-1))?;
    Ok(())
}
