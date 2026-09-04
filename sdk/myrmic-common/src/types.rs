//! Wire and utility types shared across the swarm.

pub mod ble;
/// Status and error codes passed across the Wasm ABI.
pub mod error;
/// HTTP-shaped wire types (URLs, status codes, sessions) used by the gateway
/// and bridges.
#[cfg(feature = "types-web")]
pub mod web;
