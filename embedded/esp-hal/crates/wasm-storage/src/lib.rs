//! WASM Module Storage
//!
//! This crate handles the storage of WASM modules in Flash
#![cfg_attr(target_os = "none", no_std)]
#![warn(missing_docs)]
#![warn(missing_debug_implementations)]
#![warn(unreachable_pub)]
#![warn(clippy::must_use_candidate)]
#![warn(clippy::return_self_not_must_use)]

pub mod metadata;
#[cfg(feature = "bare-metal")]
mod partitions;
#[cfg(feature = "bare-metal")]
pub mod storage;

#[cfg(feature = "bare-metal")]
pub use partitions::PartitionLayout;
#[cfg(feature = "bare-metal")]
pub use storage::*;

/// Re-export for whomever uses metadata so that postcard's versions always match
pub mod __reexports {
    pub use postcard;
}
