//! Sender cell for fire-and-forget tests.
//!
//! `trigger` sends two FF `accept` commands to the receiver, then publishes an
//! `ff_echo` event; the event arriving before the receiver's echoes proves the
//! sends are non-blocking.

#![no_std]

use myrmic_sdk::{Metadata, Result, Sri, String, publish, send};

#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
#[codec(myrmic_sdk::Postcard)]
struct Unit {}

const RECEIVER: &str = "ff_receiver_cell";

#[myrmic_sdk::cmd]
fn trigger(_md: Metadata) -> Result<()> {
    let receiver = Sri::of_path(RECEIVER).map_err(|_| "invalid sri")?;
    send(receiver, "accept", &String::from("first"))?;
    send(receiver, "accept", &String::from("second"))?;
    publish("ff_echo", &String::from("sender_done"))?;
    Ok(())
}

#[myrmic_sdk::cmd]
fn trigger_ping(_md: Metadata) -> Result<()> {
    let receiver = Sri::of_path(RECEIVER).map_err(|_| "invalid sri")?;
    send(receiver, "ping", &Unit {})?;
    publish("ff_echo", &String::from("ping_done"))?;
    Ok(())
}
