//! Cell with no `#[init]`; exposes a single no-op `ping` command.
#![no_std]

use myrmic_sdk::{Metadata, Result};

#[myrmic_sdk::cmd]
fn ping(_md: Metadata) -> Result<()> {
    Ok(())
}
