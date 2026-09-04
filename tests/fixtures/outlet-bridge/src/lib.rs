//! Outlet-bridge cell for signal-layer HIL tests — the write-side mirror of
//! `tap-bridge`.
//!
//! Exposes the two outlet command types over cell commands, so a host (or a
//! human with `myrmic send`) can drive a digital and a PWM outlet through the
//! full cell → host-function → outlet-registry chain and verify the result on
//! the physical pin:
//!
//! - **`set_gpio` command** — payload is a JSON `bool`. Writes
//!   `DigitalState { on }` to the [`GPIO_OUTLET`] outlet.
//! - **`set_pwm` command** — payload is a JSON number in `0.0..=1.0`. Writes
//!   `PwmDuty { duty }` to the [`PWM_OUTLET`] outlet.
//!
//! Commands are fire-and-forget; failures are logged on the runtime console.
//! The pipeline deployed alongside this cell must declare cell-driven outlets
//! (no `input:`) under the names below.

#![no_std]

use myrmic_sdk::outlet::Outlet;
use myrmic_sdk::{Metadata, Result, error};
use signal_layer_types::{DigitalState, PwmDuty};

/// Digital outlet this cell drives (must match the pipeline YAML).
const GPIO_OUTLET: &str = "gpio_cmd";
/// PWM outlet this cell drives (must match the pipeline YAML).
const PWM_OUTLET: &str = "pwm_cmd";

/// `set_gpio` — payload is a JSON `bool`: `true` = assert, `false` = deassert.
#[myrmic_sdk::cmd]
fn set_gpio(_md: Metadata, on: bool) -> Result<()> {
    write_outlet(GPIO_OUTLET, &DigitalState { on })
}

/// `set_pwm` — payload is a JSON number: the duty-cycle fraction `0.0..=1.0`
/// (the driver clamps to its configured range).
#[myrmic_sdk::cmd]
fn set_pwm(_md: Metadata, duty: f32) -> Result<()> {
    write_outlet(PWM_OUTLET, &PwmDuty { duty })
}

/// Resolve `name` and write the postcard-encoded `value` as its latest command.
fn write_outlet<T: serde::Serialize + myrmic_sdk::WireType>(name: &str, value: &T) -> Result<()> {
    match Outlet::resolve(name) {
        Ok(Some(outlet)) => {
            if let Err(e) = outlet.write_typed(value) {
                let _ = error!("outlet '{name}': write failed {e:?}");
                return Err("outlet write failed");
            }
            Ok(())
        }
        Ok(None) => {
            let _ = error!("outlet '{name}': not registered");
            Err("outlet not registered")
        }
        Err(e) => {
            let _ = error!("outlet '{name}': resolve error {e:?}");
            Err("outlet resolve failed")
        }
    }
}
