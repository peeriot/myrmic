//! Receives commands from object twins and forwards a command to the central

#![no_std]

use myrmic_sdk::{Message, Metadata, Result, Sri, String};

/// Must match `CENTRAL_SRI` in the benchmark driver.
const CENTRAL_SRI: &str = "bridge.central";

#[derive(serde::Serialize, serde::Deserialize, Message)]
#[codec(myrmic_sdk::Postcard)]
pub struct ZoneUpdate {
    // identify this message
    bench_id: u64,
    call_id: u64,
    // as we cannot have Vec<u8> here, the configurable sized payload is a pure ASCII string
    payload: String,
}

#[derive(serde::Serialize, serde::Deserialize, Message)]
#[codec(myrmic_sdk::Postcard)]
struct CentralUpdate {
    bench_id: u64,
    call_id: u64,
    payload: String,
}

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    Ok(())
}

#[myrmic_sdk::cmd]
fn update(_md: Metadata, update: ZoneUpdate) -> Result<()> {
    let ZoneUpdate {
        bench_id,
        call_id,
        payload,
    } = update;
    let _ = myrmic_sdk::info!("TIER-2-BENCH-{bench_id}-CALL-{call_id}");

    // A command, not an event: an event goes into one `@events/<name>` scope
    // shared by every publisher, so whichever node holds it is a cross-node
    // write for all the others. A command goes into the recipient's own
    // mailbox scope, which is the scope this benchmark pre-allocates onto the
    // host that runs the recipient.
    let target = Sri::of_path(CENTRAL_SRI).map_err(|_| "invalid central sri")?;
    myrmic_sdk::send(
        target,
        "central_update",
        &CentralUpdate {
            bench_id,
            call_id,
            payload,
        },
    )?;

    Ok(())
}
