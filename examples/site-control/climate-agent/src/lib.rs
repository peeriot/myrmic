#![no_std]

use myrmic_sdk::db::state::State;
use myrmic_sdk::{Metadata, Result, Sri, publish, send};

const CONTROLLER: &str = "controller";

const HEATING: State<bool> = State::new_const("heating");

#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
struct SiteState {
    temperature: f32,
    heating: bool,
    target_low: f32,
    target_high: f32,
}

#[myrmic_sdk::evt]
fn site_state(_md: Metadata, site: SiteState) -> Result<()> {
    let heating = HEATING.load()?.unwrap_or_default();

    let desired = if !heating && site.temperature <= site.target_low {
        true
    } else if heating && site.temperature >= site.target_high {
        false
    } else {
        return Ok(());
    };

    controller(desired)?;
    HEATING.save(&desired)?;
    publish("heating_requested", &desired)?;

    Ok(())
}

fn controller(on: bool) -> Result<()> {
    let controller = Sri::of_path(CONTROLLER).map_err(|_| "invalid controller srn")?;

    send(controller, "set_heating", &on)
}
