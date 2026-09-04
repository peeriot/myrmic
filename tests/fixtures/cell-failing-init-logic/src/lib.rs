//! Cell whose `#[init]` always fails — used to test deploy-failure handling.
#![no_std]

use myrmic_sdk::{Metadata, Result};

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    Err("init deliberately failed")
}
