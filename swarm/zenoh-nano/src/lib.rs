//! Zenoh Layer support
#![no_std]
#![deny(missing_docs)]
#![allow(clippy::uninlined_format_args)] // For `defmt`

extern crate alloc;

/// Re-export zenoh-buffers because `ZSlice` is part of the public API
pub mod buffers {
    pub use zenoh_buffers::*;
}

// Brings the log-level, assert and `unwrap!` macros into crate-wide scope, the way
// the vendored `fmt` module used to. A per-module `use` would leave any module that
// forgot it silently falling back to `core::assert!` and friends.
#[macro_use]
extern crate defmt_or_log;

pub(crate) mod fmt;

pub mod clock;
pub mod dispatch;
pub mod link;
pub mod network;
pub mod ops;
pub mod rng;
pub mod scout;
pub mod session;
pub mod transport;
