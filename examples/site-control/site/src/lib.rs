#![no_std]

use myrmic_sdk::db::state::State;
use myrmic_sdk::{Metadata, Result, publish};

#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
struct Site {
    temperature: f32,
    heating: bool,
    target_low: f32,
    target_high: f32,
}

impl Default for Site {
    fn default() -> Self {
        Self {
            temperature: 0.0,
            heating: false,
            target_low: 26.0,
            target_high: 30.0,
        }
    }
}

const SITE: State<Site> = State::new_const("site");

#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
struct TargetRange {
    low: f32,
    high: f32,
}

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    let site = SITE.load()?.unwrap_or_default();

    SITE.save(&site)
}

#[myrmic_sdk::evt]
fn temperature(_md: Metadata, value: f32) -> Result<()> {
    let site = SITE.upsert_with(|site| site.temperature = value)?;

    publish("site_state", &site)
}

#[myrmic_sdk::evt]
fn heating_state(_md: Metadata, on: bool) -> Result<()> {
    let site = SITE.upsert_with(|site| site.heating = on)?;

    publish("site_state", &site)
}

#[myrmic_sdk::cmd]
fn set_target(_md: Metadata, range: TargetRange) -> Result<()> {
    let site = SITE.upsert_with(|site| {
        site.target_low = range.low;
        site.target_high = range.high;
    })?;

    publish("site_state", &site)
}
