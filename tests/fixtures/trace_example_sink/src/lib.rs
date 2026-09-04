#![no_std]

use myrmic_sdk::{Bytes, Metadata, Result};

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    myrmic_sdk::info!("trace-example-sink initialized").ok();
    Ok(())
}

#[myrmic_sdk::cmd]
fn sink(_md: Metadata, _payload: Bytes) -> Result<()> {
    myrmic_sdk::info!("sink command received").ok();

    Ok(())
}
