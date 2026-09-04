//! Crate exposing the API to interact with the self-organization layer of swarm.

mod cells;
mod client;
mod config;
mod error;
mod orchestration;
mod runtimes;

pub mod types;
pub mod utils;

pub use cells::EventQueue;
pub use client::Client;
pub use config::Config;
pub use error::{Error, Result};
