//! The Modbus TCP bridge: polls a Modbus server and exchanges values with cells.
//!
//! Built in layers, each on top of the one before:
//!
//! - `codec`: typed values to and from raw bits and 16-bit words,
//! - `connection`: the one connection to the server, with reconnect and timeout,
//! - `point`: reading and writing the typed value of a spec entry,
//! - `poll` and `command`: what `poll`, `read` and `write` entries do,
//! - `handle`: the bridge cell that wires it all to the swarm, [`ModbusBridgeHandle`].

mod codec;
mod command;
mod connection;
mod handle;
mod point;
mod poll;

pub use handle::ModbusBridgeHandle;
