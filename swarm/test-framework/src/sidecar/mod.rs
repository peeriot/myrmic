//! client and server halves of the test-sidecar HTTP API
//!
//! The server runs inside the system under test (usually a compose network) and is
//! deployed as a thin binary wrapper, see `e2e-tests/src/bin/test-sidecar.rs`. The
//! client drives it from the test side.

pub mod client;
pub mod server;

pub use client::*;
