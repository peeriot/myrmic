//! Receives commands/events from producers and delegates them to zone twins

#![no_std]

use myrmic_sdk::{Message, Metadata, Result, Sri, String, format};

#[derive(serde::Serialize, serde::Deserialize, Message)]
#[codec(myrmic_sdk::Postcard)]
pub struct ObjectUpdate {
    // identify this message
    bench_id: u64,
    call_id: u64,
    // further routing
    zone_id: u16,
    // as we cannot have Vec<u8> here, the configurable sized payload is a pure ASCII string
    payload: String,
}

#[derive(serde::Serialize, serde::Deserialize, Message)]
#[codec(myrmic_sdk::Postcard)]
struct ZoneUpdate {
    bench_id: u64,
    call_id: u64,
    payload: String,
}

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    Ok(())
}

#[myrmic_sdk::cmd]
fn update(_md: Metadata, update: ObjectUpdate) -> Result<()> {
    let ObjectUpdate {
        bench_id,
        call_id,
        zone_id,
        payload,
    } = update;
    let _ = myrmic_sdk::info!("TIER-1-BENCH-{bench_id}-CALL-{call_id}");

    let target = Sri::of_path(&format!("agent.zone.{zone_id}")).map_err(|_| "invalid zone sri")?;
    myrmic_sdk::send(
        target,
        "update",
        &ZoneUpdate {
            bench_id,
            call_id,
            payload,
        },
    )?;

    Ok(())
}
