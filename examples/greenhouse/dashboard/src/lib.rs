//! Dashboard adapter: serves the greenhouse web page through the gateway.
//! It mirrors the grow-bed's `bed_state` into a small JSON snapshot that the
//! page polls. It owns no truth and makes no decisions.
//!
//! Part 5 of the Smart Greenhouse tutorial.
#![no_std]

use myrmic_sdk::{Metadata, Result, format, gateway};

/// Payload of the grow-bed's `bed_state` event. Declared here as well as in the
/// grow-bed: cells share wire formats, not Rust types.
#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
struct BedState {
    moisture: f32,
    pump_on: bool,
    target_low: f32,
    target_high: f32,
}

#[myrmic_sdk::init]
fn init(md: Metadata) -> Result<()> {
    gateway::assets(md.id).put("/index.html", include_bytes!("../assets/index.html"))?;
    gateway::mount("/greenhouse")
        .index("/index.html")
        .bind()
        .map_err(<&'static str>::from)?;

    Ok(())
}

/// Every `bed_state` update becomes a fresh `latest.json` for the page to poll.
#[myrmic_sdk::evt]
fn bed_state(md: Metadata, bed: BedState) -> Result<()> {
    let json = format!(
        r#"{{"moisture":{:.1},"pump_on":{},"target_low":{},"target_high":{}}}"#,
        bed.moisture, bed.pump_on, bed.target_low, bed.target_high
    );

    gateway::assets(md.id).put("/latest.json", json.as_bytes())
}
