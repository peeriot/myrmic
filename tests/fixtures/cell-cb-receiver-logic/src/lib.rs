//! Receiver cell for callback command tests.
//!
//! Each command publishes an observable `cb_echo` event. `accept` then always
//! invokes the caller's callback; `ping` takes an optional one and invokes it
//! only when there is a callback and a sender to answer.

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
fn ping(md: Metadata, cb: Option<Callback<Unit>>) -> Result<()> {
    publish("cb_echo", &String::from("pong"))?;

    // `myrmic send` carries no callback, and a nil sender that could not be
    // invoked even if it did.
    if let Some(cb) = cb
        && !md.sender.is_nil()
    {
        cb.invoke(md.sender, &Unit {})?;
    }

    Ok(())
}
