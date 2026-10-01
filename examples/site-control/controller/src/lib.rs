#![no_std]

use core::time::Duration;

use myrmic_sdk::outlet::Outlet;
use myrmic_sdk::signal_layer::DigitalState;
use myrmic_sdk::tap::Tap;
use myrmic_sdk::{Callback, Metadata, Result, error, publish, warn};

const TEMPERATURE_TAP: &str = "temperature";
const HEAT_OUTLET: &str = "heat_relay";

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    let _ = myrmic_sdk::interval(Callback::of::<sample>(), Duration::from_secs(1))
        .build()
        .map_err(|_| "timer failed")?;
    Ok(())
}

#[myrmic_sdk::cmd]
fn sample(_md: Metadata) -> Result<()> {
    let tap = match Tap::resolve(TEMPERATURE_TAP) {
        Ok(Some(tap)) => tap,
        Ok(None) => {
            let _ = warn!("tap '{TEMPERATURE_TAP}' is not registered on this node");
            return Ok(());
        }
        Err(e) => {
            let _ = error!("tap '{TEMPERATURE_TAP}': resolve failed: {e:?}");
            return Err("tap resolve failed");
        }
    };

    let (_ts_ms, value) = match tap.read_typed::<f32>() {
        Ok(Some(read)) => read,
        Ok(None) => return Ok(()),
        Err(e) => {
            let _ = error!("tap '{TEMPERATURE_TAP}': read failed: {e:?}");
            return Err("tap read failed");
        }
    };

    publish("temperature", &value)
}

#[myrmic_sdk::cmd]
fn set_heating(_md: Metadata, on: bool) -> Result<()> {
    let applied = match Outlet::resolve(HEAT_OUTLET) {
        Ok(Some(outlet)) => match outlet.write_typed(&DigitalState { on }) {
            Ok(()) => true,
            Err(e) => {
                let _ = error!("outlet '{HEAT_OUTLET}': write failed: {e:?}");
                false
            }
        },
        Ok(None) => {
            let _ = error!("outlet '{HEAT_OUTLET}': not registered on this node");
            false
        }
        Err(e) => {
            let _ = error!("outlet '{HEAT_OUTLET}': resolve failed: {e:?}");
            false
        }
    };

    publish("heating_state", &(applied && on))
}
