#![no_std]

use core::time::Duration;

use myrmic_sdk::{Metadata, Result};

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    myrmic_sdk::wait(Duration::from_secs(12)).map_err(|_| "wait failed")?;
    Ok(())
}
