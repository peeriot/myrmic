//! Watchdog self-test cell (HIL tests only).
//!
//! Deployed to firmware built with the `wdt-selftest` feature, this cell lets a
//! HIL test deliberately trigger a liveness stall so the on-die watchdog can be
//! exercised end-to-end (SDS-FEAT-2026-HWD-001). Each command calls the host
//! `selftest` import, which records the request; the firmware's prio-1 stats
//! task then performs the actual wedge (a cell at WAMR prio-0 cannot starve the
//! prio-1 executor the watchdog protects, so the host side does the wedging).
//!
//! The subsequent hardware reset is observed by the test via the swarm
//! watchdog-reset report, not from this cell.

#![no_std]

use myrmic_sdk::{EventPublishRequest, Metadata, Result, publish_event};

/// Event the [`ping`] command answers on. Commands are fire-and-forget, so a
/// cell reports back by publishing rather than returning a value.
const ALIVE_EVENT: &str = "wdt_selftest_alive";

/// Host `selftest` import — registered only when the firmware is built with the
/// `wdt-selftest` feature.
mod host {
    #[link(wasm_import_module = "selftest")]
    unsafe extern "C" {
        /// Request a deliberate liveness wedge: `1` = spin the executor, `2` =
        /// stall a required task. Returns a status code.
        pub(super) fn wdt_selftest_wedge(mode: i32) -> i32;
    }
}

/// Request a wedge from the host. Free function so the command bodies carry no
/// `unsafe`.
fn request_wedge(mode: i32) {
    // SAFETY: FFI call to the host `selftest` import; it only stores a flag.
    let _ = unsafe { host::wdt_selftest_wedge(mode) };
}

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    Ok(())
}

/// Benign liveness probe — publishes `1` on [`ALIVE_EVENT`] while the cell is
/// alive. Used to confirm the device did *not* reset during a no-fault soak: a
/// reset boots the node clean and drops the cell, so no event arrives.
#[myrmic_sdk::cmd]
fn ping(_md: Metadata) -> Result<()> {
    publish_event(&EventPublishRequest {
        event: ALIVE_EVENT.try_into()?,
        payload: Some(postcard::to_allocvec(&1i32).map_err(|_| "encode ping payload")?),
    })?;
    Ok(())
}

/// Wedge the whole prio-1 executor (spin, never yields) — drives the staged
/// MWDT to `ResetSystem` ~45 s later (`last_reason == MwdtStaged`).
#[myrmic_sdk::cmd]
fn wedge_spin(_md: Metadata) -> Result<()> {
    request_wedge(1);
    Ok(())
}

/// Stall a required liveness task (parks forever; the executor stays alive)
/// — the supervisor detects the stall and withholds the feed, so the MWDT
/// resets and the report names the stalled task.
#[myrmic_sdk::cmd]
fn wedge_stall(_md: Metadata) -> Result<()> {
    request_wedge(2);
    Ok(())
}
