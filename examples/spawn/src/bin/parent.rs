//! Parent cell: spawns N children of the `child` class on command.
//!
//! `declare!("child")` embeds a placeholder in this binary that the deploy
//! toolchain patches with the `child` class's content hash, so the parent
//! spawns by a stable hash rather than a name resolved at runtime.
//!
//! `parent` and `child` are two binaries of the same crate; each is built as
//! its own class (`parent` / `child`) — see `app-specs.yml`.
#![no_std]
#![no_main]

use myrmic_sdk::{ClassHandle, Metadata};

/// Reference to the `child` class, resolved to a content hash at deploy time.
const CHILD: ClassHandle = myrmic_sdk::declare!("child");

#[myrmic_sdk::init]
fn init(md: Metadata) -> myrmic_sdk::Result {
    let _ = myrmic_sdk::info!("parent started (id={:?})", md.id).ok();
    Ok(())
}

/// Spawns `count` children, each under a distinct local name.
///
/// `m send <parent> spawn 3`
#[myrmic_sdk::cmd]
fn spawn(_md: Metadata, count: serde_json::Number) -> myrmic_sdk::Result {
    let count = count
        .as_u64()
        .ok_or("count must be a non-negative integer")?;

    let _ = myrmic_sdk::info!("spawning {} child(ren)", count).ok();

    for i in 0..count {
        let sri = CHILD.spawn()?;

        let _ = myrmic_sdk::info!("spawned child {:?}", sri).ok();
    }
    Ok(())
}
