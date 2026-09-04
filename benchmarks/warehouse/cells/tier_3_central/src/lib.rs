//! Receives commands from all zone twins

#![no_std]

use myrmic_sdk::{Message, Metadata, Result, String};

#[derive(serde::Serialize, serde::Deserialize, Message)]
#[codec(myrmic_sdk::Postcard)]
pub struct CentralUpdate {
    // identify this message
    bench_id: u64,
    call_id: u64,
    // as we cannot have Vec<u8> here, the configurable sized payload is a pure ASCII string
    payload: String,
}

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    Ok(())
}

#[myrmic_sdk::cmd]
fn central_update(_md: Metadata, update: CentralUpdate) -> Result<()> {
    let CentralUpdate {
        bench_id,
        call_id,
        payload: _,
    } = update;

    let _ = myrmic_sdk::info!("TIER-3-BENCH-{bench_id}-CALL-{call_id}");

    Ok(())
}
