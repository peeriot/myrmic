//! Simple example cell used by the sorg integration tests.
#![no_std] // required for wasm32-unknown-unknown

use myrmic_sdk::db::tree::Kv;
use myrmic_sdk::{Metadata, Result};

/// Marker the `output` command writes into the cell's private KV store. Commands are
/// fire-and-forget (no reply value), so a test confirms a deployed cell received
/// `output` by reading this key back — stored key `"dummy/output"` — instead of
/// waiting on a response.
const MARKER: Kv<bool> = Kv::new("dummy");

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    Ok(())
}

/// Fire-and-forget command; records that it ran by writing a marker to the cell's own
/// KV store for a test to observe.
#[myrmic_sdk::cmd]
fn output(_md: Metadata) -> Result<()> {
    MARKER.put("output", &true)?;
    Ok(())
}
