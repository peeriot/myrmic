//! Spawner cell: spawns and terminates child cells on command.
//!
//! `spawn` takes a postcard-encoded [`SpawnRequest`] payload; `terminate` takes a
//! postcard-encoded SRI string. Both are fire-and-forget — success/failure is
//! observed by the host via the cell registry, not a command reply.
#![no_std]

use myrmic_sdk::{Bytes, Codec, Metadata, Postcard, Result, SpawnRequest, String};

/// Spawns a child cell from a postcard-encoded `SpawnRequest` payload.
#[myrmic_sdk::cmd]
fn spawn(_md: Metadata, payload: Bytes) -> Result<()> {
    let request: SpawnRequest = Postcard::decode(&payload)?;
    myrmic_sdk::spawn_cell(&request)?;
    Ok(())
}

/// Terminates the child whose SRI is the postcard-encoded string payload.
#[myrmic_sdk::cmd]
fn terminate(_md: Metadata, payload: Bytes) -> Result<()> {
    let sri: String = Postcard::decode(&payload)?;
    myrmic_sdk::terminate_cell(&sri)?;
    Ok(())
}
