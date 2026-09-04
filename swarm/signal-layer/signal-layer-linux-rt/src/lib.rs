//! Runtime support for generated Linux pipelines: fenced time seam, IPC-server
//! bootstrap, and the outlet-store handoff between the generated setup functions.

mod outlet_handoff;
mod server;
pub mod time;

pub use outlet_handoff::{set_outlet_store, take_outlet_store};
pub use server::{run_signal_server, run_tap_server};
