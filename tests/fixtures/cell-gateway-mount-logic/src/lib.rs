//! Cell that declares gateway resources in `#[init]`: one asset in its own
//! store and a mount serving it. The orchestration tests check both are
//! released when the cell goes away.

#![no_std]

use myrmic_sdk::{Metadata, Result, gateway};

/// Mirrored by the orchestration gateway tests.
const MOUNT: &str = "/gateway-mount-test";
const INDEX: &str = "/index.html";

#[myrmic_sdk::init]
fn init(md: Metadata) -> Result<()> {
    gateway::assets(md.id).put(INDEX, b"<h1>gateway mount test</h1>")?;
    gateway::mount(MOUNT).api("/api").index(INDEX).bind()?;
    Ok(())
}
