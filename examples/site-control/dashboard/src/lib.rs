#![no_std]

use myrmic_sdk::gateway::{self, Asset, HttpRequest};
use myrmic_sdk::{Callback, Metadata, Result, format};

#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
struct SiteState {
    temperature: f32,
    heating: bool,
    target_low: f32,
    target_high: f32,
}

#[myrmic_sdk::init]
fn init(md: Metadata) -> Result<()> {
    gateway::assets(md.id).put("/index.html", include_bytes!("../assets/index.html"))?;
    gateway::mount("/site-control", Callback::of::<on_http>())?;

    Ok(())
}

/// Every request is a file from the asset store.
#[myrmic_sdk::cmd]
fn on_http(_md: Metadata, req: HttpRequest<'_>) -> Result<()> {
    let asset = Asset::new(req.path).index("/index.html");
    req.asset(asset)
}

#[myrmic_sdk::evt]
fn site_state(md: Metadata, site: SiteState) -> Result<()> {
    let json = format!(
        r#"{{"temperature":{:.1},"heating":{},"target_low":{},"target_high":{}}}"#,
        site.temperature, site.heating, site.target_low, site.target_high
    );

    gateway::assets(md.id).put("/latest.json", json.as_bytes())
}
