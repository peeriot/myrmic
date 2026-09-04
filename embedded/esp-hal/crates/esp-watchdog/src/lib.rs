//! Firmware task-liveness monitoring: liveness detection ([`liveness`]), on-die
//! watchdog enforcement ([`watchdog`]) and heap-usage snapshots ([`report`]).
//!
//! The watchdog feeder is a plain `async fn` — the firmware binary wraps it in its
//! own `#[embassy_executor::task]` and does all spawning, so the
//! embassy-executor version stays a firmware-side choice.

#![no_std]

extern crate alloc;

pub mod liveness;
pub mod report;
pub mod watchdog;
