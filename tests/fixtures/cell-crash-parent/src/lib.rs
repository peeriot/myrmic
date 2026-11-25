//! Parent for the handler-crash tests: spawns a child on command and stores the
//! reason its monitor handler is given when the child is lost, so a test can
//! read what the runtime reported.
#![no_std]

use myrmic_sdk::db::tree::Kv;
use myrmic_sdk::monitor::{CellLost, LostReason};
use myrmic_sdk::{Bytes, Codec, Metadata, Postcard, Result, SpawnRequest};

const KV: Kv<LostReason> = Kv::new("crash_parent");
const LOST: &str = "lost";

/// Spawns a child from a postcard-encoded `SpawnRequest` payload.
#[myrmic_sdk::cmd]
fn spawn(_md: Metadata, payload: Bytes) -> Result<()> {
    let request: SpawnRequest = Postcard::decode(&payload)?;
    myrmic_sdk::spawn_cell(&request)?;
    Ok(())
}

/// Stores why the child was lost.
#[myrmic_sdk::monitor]
fn lost(_md: Metadata, lost: CellLost) -> Result<()> {
    KV.put(LOST, &lost.reason)
}
