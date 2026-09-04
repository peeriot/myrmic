//! Sender cell for callback command tests.
//!
//! Each trigger sends a callback to the receiver and publishes an immediate
//! `cb_echo`; the receiver later invokes the named callback handler here.

#![no_std]

use myrmic_sdk::{Callback, Metadata, Result, Sri, String, publish, send};

#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
#[codec(myrmic_sdk::Postcard)]
struct CbResp {}

#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
#[codec(myrmic_sdk::Postcard)]
struct Unit {}

const RECEIVER: &str = "cb_receiver_cell";

#[myrmic_sdk::cmd]
fn trigger(_md: Metadata) -> Result<()> {
    let receiver = Sri::of_path(RECEIVER).map_err(|_| "invalid sri")?;
    send(receiver, "accept", &Callback::of::<on_reply>())?;
    publish("cb_echo", &String::from("sender_immediate"))?;
    Ok(())
}

#[myrmic_sdk::cmd]
fn trigger_ping(_md: Metadata) -> Result<()> {
    let receiver = Sri::of_path(RECEIVER).map_err(|_| "invalid sri")?;
    send(receiver, "ping", &Callback::of::<on_ping>())?;
    publish("cb_echo", &String::from("sender_immediate"))?;
    Ok(())
}

#[myrmic_sdk::cmd]
fn on_reply(_md: Metadata, _r: CbResp) -> Result<()> {
    publish("cb_echo", &String::from("callback_done"))?;
    Ok(())
}

#[myrmic_sdk::cmd]
fn on_ping(_md: Metadata, _u: Unit) -> Result<()> {
    publish("cb_echo", &String::from("ping_done"))?;
    Ok(())
}
