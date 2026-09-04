//! Child cell: logs on startup. Instances are spawned dynamically by `parent`.
//!
//! One of two binaries in this crate; built as the `child` class (`child`).
#![no_std]
#![no_main]

use myrmic_sdk::Metadata;

#[myrmic_sdk::init]
fn init(md: Metadata) -> myrmic_sdk::Result {
    let _ = myrmic_sdk::info!("child started (id={:?})", md.id).ok();
    Ok(())
}
