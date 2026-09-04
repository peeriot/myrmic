//! The firmware-side cell service: the node's workload pump over the data
//! layer — deployment polling and transfer, the cell mailbox, guest DB request
//! dispatch, cell supervision (leases, fencing) and exec-registry registration.
//!
//! [`service()`] is a plain `async fn` — the firmware binary wraps it in its own
//! `#[embassy_executor::task]` and does all spawning.

#![no_std]

extern crate alloc;

mod deploy;
mod mailbox;
mod myrmic;
mod requests;
mod service;
mod supervision;

pub use service::service;
