//! Receiver cell for callback command tests.
//!
//! Each command publishes an observable `cb_echo` event and then invokes the
//! caller's callback (on `md.sender`) with a reply payload.

#![no_std]

use core::time::Duration;

use myrmic_sdk::{Callback, Metadata, Result, String, publish};

#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
#[codec(myrmic_sdk::Postcard)]
struct CbResp {}

#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
#[codec(myrmic_sdk::Postcard)]
struct Unit {}

#[myrmic_sdk::cmd]
fn accept(md: Metadata, cb: Callback<CbResp>) -> Result<()> {
    myrmic_sdk::wait(Duration::from_millis(10))?;
    publish("cb_echo", &String::from("receiver_done"))?;
    cb.invoke(md.sender, &CbResp {})?;
    Ok(())
}

#[myrmic_sdk::cmd]
fn ping(md: Metadata, cb: Callback<Unit>) -> Result<()> {
    publish("cb_echo", &String::from("pong"))?;
    cb.invoke(md.sender, &Unit {})?;
    Ok(())
}
