//! Non-yielding production cell (HIL tests only).
//!
//! Fixture for `REQ-FEAT-2026-HWD-SEC-002`: a production-profile cell running a
//! computation that never yields must not be able to stop the liveness
//! supervisor feeding the watchdog.
//!
//! Unlike `cell-wdt-selftest-logic` this uses no privileged import and asks the
//! host for nothing. That is the point: the runtime schedules a cell on the
//! WAMR thread at priority 0, below the priority-1 executor carrying zenoh, the
//! wasm request handler and stats (`modem-esp32/src/main.rs`), so a preemptive
//! scheduler keeps the protected tasks running however long this loop spins.
//! The self-test cell has to route its wedge through a host import precisely
//! because a cell cannot do it from here.

#![no_std]

use core::time::Duration;

use myrmic_sdk::{EventPublishRequest, Metadata, Result, publish_event};

/// Answered by [`ping`] while the cell is alive. A watchdog reset boots the
/// node clean and drops the cell, so silence here means the device reset.
const ALIVE_EVENT: &str = "wdt_spin_alive";

/// Published by [`spin`] once the busy loop has run its full duration.
const DONE_EVENT: &str = "wdt_spin_done";

/// Iterations of arithmetic between two clock reads. Large enough that the loop
/// is a computation rather than a poll on the host clock.
const WORK_PER_CHECK: u64 = 10_000;

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    Ok(())
}

/// Liveness probe. Publishes `1` on [`ALIVE_EVENT`], so a test can tell the
/// difference between a device that survived and one that reset and came back
/// without the cell.
#[myrmic_sdk::cmd]
fn ping(_md: Metadata) -> Result<()> {
    publish_event(&EventPublishRequest {
        event: ALIVE_EVENT.try_into()?,
        payload: Some(postcard::to_allocvec(&1i32).map_err(|_| "encode ping payload")?),
    })?;
    Ok(())
}

/// Occupy the WAMR thread for `seconds` without ever yielding, then report the
/// accumulator on [`DONE_EVENT`].
///
/// The accumulator is published rather than discarded so the loop cannot be
/// optimised away: a busy loop with an unused result is not a busy loop.
#[myrmic_sdk::cmd]
fn spin(_md: Metadata, seconds: u32) -> Result<()> {
    let deadline =
        myrmic_sdk::now().map_err(|_| "now() failed")? + Duration::from_secs(u64::from(seconds));

    let mut acc: u64 = 0;
    loop {
        for i in 0..WORK_PER_CHECK {
            acc = acc.wrapping_add(i ^ acc);
        }
        if myrmic_sdk::now().map_err(|_| "now() failed")? >= deadline {
            break;
        }
    }

    publish_event(&EventPublishRequest {
        event: DONE_EVENT.try_into()?,
        payload: Some(postcard::to_allocvec(&acc).map_err(|_| "encode done payload")?),
    })?;
    Ok(())
}
