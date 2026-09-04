//! Receiver cell for fire-and-forget tests.
//!
//! On `accept` it waits briefly then echoes the payload back as an `ff_echo`
//! event. The delay makes ordering observable: the sender's event arrives
//! before the receiver's echoes.

#![no_std]

use core::time::Duration;

use myrmic_sdk::{Metadata, Result, Sri, String, publish};

#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
#[codec(myrmic_sdk::Postcard)]
struct FfSender {
    sender: Sri,
}

#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
#[codec(myrmic_sdk::Postcard)]
struct Unit {}

#[myrmic_sdk::cmd]
fn accept(md: Metadata, arg: String) -> Result<()> {
    myrmic_sdk::wait(Duration::from_millis(10))?;
    publish("ff_sender", &FfSender { sender: md.sender })?;
    publish("ff_echo", &arg)?;
    Ok(())
}

#[myrmic_sdk::cmd]
fn ping(_md: Metadata, _u: Unit) -> Result<()> {
    myrmic_sdk::wait(Duration::from_millis(10))?;
    publish("ff_echo", &String::from("pong"))?;
    Ok(())
}
