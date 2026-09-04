//! Network bring-up for the myrmic firmware: WiFi station management, the
//! embassy-net stack, zenoh scouting/session supervision and the zenoh request
//! adapter.
//!
//! Every entry point is a plain `async fn` (or a sync constructor) — the
//! firmware binary wraps them in its own `#[embassy_executor::task]`s and does
//! all spawning. The established `Session` is handed back through a signal so
//! the binary can start the session-scoped services itself.

#![no_std]

extern crate alloc;

mod clock;
mod session;
mod zenoh_client;

pub use clock::wall_time;
pub use session::{CONNECTED, SESSION_LEASE, connection, init_stack, zenoh_session};
pub use zenoh_client::client as zenoh_client;
pub use zenoh_nano::session::Session;
