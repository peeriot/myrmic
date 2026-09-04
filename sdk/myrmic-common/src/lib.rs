//! Types and helpers shared between the Myrmic host runtime and the cell SDK:
//! cell identity and messaging, datalayer wire types, gateway routing, and the
//! signal-layer type re-exports. `myrmic-sdk` re-exports the guest-facing
//! parts; the host runtime consumes the same definitions, so both sides of
//! the Wasm ABI agree by construction.

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]
#![allow(clippy::pedantic)] // temporary: disable pedantic lint gate for myrmic-common
#![allow(clippy::cast_possible_truncation)] // using the crate just from Wasm -> no need to worry about casting to i32
#![allow(clippy::cast_possible_wrap)] // using the crate just from Wasm -> no need to worry about casting to i32

#[cfg(feature = "alloc")]
#[allow(unused_extern_crates)]
// needed by the alloc-only modules; unused in codegen-only (std) builds
extern crate alloc;

#[cfg(feature = "cells")]
pub mod cells;
#[cfg(feature = "codegen")]
#[allow(missing_docs)] // host-only codegen internals, not rendered API docs
pub mod codegen;
#[cfg(feature = "db")]
pub mod db;
#[cfg(feature = "db")]
pub mod gateway;

/// Re-exports `myrmic-signal-layer-types` as `signal_layer`, making it part of
/// this crate's public API: a consumer mixing two `myrmic-common` majors built
/// against different `myrmic-signal-layer-types` majors gets an unfixable type
/// mismatch.
pub use signal_layer_types as signal_layer;

pub mod types;
