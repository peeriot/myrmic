//! Module containing the host functions required to implement cell modules according to the SCCA system architecture.

mod commands;
mod events;
mod spawning;
mod timers;

pub use commands::{CommandError, send_command};
pub use events::publish_event;
pub use myrmic_common::cells::{ClassRef, SpawnError, SpawnRequest, TerminateError};
pub use spawning::{ClassHandle, SpawnBuilder, spawn_cell, stop_self, terminate_cell};
pub use timers::{TimerHandle, delay, interval, interval_at};
