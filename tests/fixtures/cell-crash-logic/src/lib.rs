//! Cell for the handler-crash tests: its handlers panic or return an error on
//! demand, and its init counts incarnations in persistent state, so a test can
//! tell a restarted cell from the same instance running on.
#![no_std]

use core::time::Duration;

use myrmic_sdk::db::tree::Kv;
use myrmic_sdk::{Callback, Metadata, Result};

const KV: Kv<i32> = Kv::new("crash");
/// How many times init ran: one per incarnation.
const INCARNATION: &str = "incarnation";
/// How many `bump` commands committed.
const HANDLED: &str = "handled";
/// The incarnation `panic_once` finally succeeded in.
const PANIC_ONCE: &str = "panic_once";
/// Written by `panic_once` right before it panics, so it never commits.
const CRASHED_WRITE: &str = "crashed_write";

fn stored(key: &str) -> Result<i32> {
    Ok(KV.get(key)?.unwrap_or(0))
}

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    KV.put(INCARNATION, &(stored(INCARNATION)? + 1))
}

#[myrmic_sdk::cmd]
fn bump(_md: Metadata) -> Result<()> {
    KV.put(HANDLED, &(stored(HANDLED)? + 1))
}

/// Returns an error: a handler error, not a crash.
#[myrmic_sdk::cmd]
fn fail(_md: Metadata) -> Result<()> {
    Err("fail: deliberate handler error")
}

/// Panics on every delivery.
#[myrmic_sdk::cmd]
fn panic_always(_md: Metadata) -> Result<()> {
    panic!("panic_always: deliberate panic");
}

/// Panics in the first incarnation only, after a write that must not survive
/// the crash. Delivered again to a later incarnation, it succeeds and records
/// which one.
#[myrmic_sdk::cmd]
fn panic_once(_md: Metadata) -> Result<()> {
    let incarnation = stored(INCARNATION)?;
    if incarnation == 1 {
        KV.put(CRASHED_WRITE, &incarnation)?;
        panic!("panic_once: deliberate panic in the first incarnation");
    }
    KV.put(PANIC_ONCE, &incarnation)
}

/// Panics on every event.
#[myrmic_sdk::evt]
fn crash_evt(_md: Metadata) -> Result<()> {
    panic!("crash_evt: deliberate panic");
}

/// Succeeds and counts itself: a marker that follows `fail_evt`.
#[myrmic_sdk::evt]
fn mark_evt(_md: Metadata) -> Result<()> {
    KV.put(HANDLED, &(stored(HANDLED)? + 1))
}

/// Returns an error on every event.
#[myrmic_sdk::evt]
fn fail_evt(_md: Metadata) -> Result<()> {
    Err("fail_evt: deliberate handler error")
}

/// Arms a one-shot timer whose tick panics.
#[myrmic_sdk::cmd]
fn arm_panic_timer(_md: Metadata) -> Result<()> {
    let _timer =
        myrmic_sdk::delay(Callback::of::<timer_panic>(), Duration::from_millis(100)).build()?;
    Ok(())
}

/// Arms a one-shot timer whose tick returns an error, then one that succeeds
/// after it, so the second tick proves the first one ran.
#[myrmic_sdk::cmd]
fn arm_fail_timer(_md: Metadata) -> Result<()> {
    let _fail =
        myrmic_sdk::delay(Callback::of::<timer_fail>(), Duration::from_millis(100)).build()?;
    let _mark = myrmic_sdk::delay(Callback::of::<bump>(), Duration::from_millis(300)).build()?;
    Ok(())
}

#[myrmic_sdk::cmd]
fn timer_panic(_md: Metadata) -> Result<()> {
    panic!("timer_panic: deliberate panic");
}

/// Timer target that returns an error.
#[myrmic_sdk::cmd]
fn timer_fail(_md: Metadata) -> Result<()> {
    Err("timer_fail: deliberate handler error")
}
