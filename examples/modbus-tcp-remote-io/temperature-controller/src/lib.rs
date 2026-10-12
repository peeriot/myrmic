#![no_std]

use core::time::Duration;

use myrmic_sdk::{self as sdk, Callback, Metadata, Result, String, TimerHandle, db::state::State};
use serde::{Deserialize, Serialize};

sdk::import!("../ur20-modbus-tcp-bridge.yml");

const UR20_CLIENT: Ur20Client = Ur20Client::new("ur20");

const FAN_ON_ABOVE_CELSIUS: f32 = 28.0;
const FAN_OFF_BELOW_CELSIUS: f32 = 26.0;

const FAN_STATE: State<bool> = State::new_const("fan");

const HEATER_OFF_ABOVE_CELSIUS: f32 = 45.0;
const HEATING_TIME: Duration = Duration::from_secs(60);

const HEATER_STATE: State<bool> = State::new_const("heater");
const HEAT_TIMER: State<Option<TimerHandle>> = State::new_const("heat_timer");

const INPUT_DELAY_10_MS: u16 = 3;

const MEASUREMENT_RANGE_PT100: u16 = 0;
const CONNECTION_TYPE_TWO_WIRE: u16 = 0;

struct Module {
    name: &'static str,
    id: u32,
}

const RTD_MODULE: Module = Module {
    name: "UR20-4AI-RTD-DIAG",
    id: 0x0406_1544,
};
const DO_MODULE: Module = Module {
    name: "UR20-4DO-P-2A",
    id: 0x0105_2FA0,
};

#[sdk::evt]
fn rtd_module(_md: Metadata, event: RtdModule) -> Result<()> {
    warn_if_unexpected(&RTD_MODULE, event.id);
    Ok(())
}

#[sdk::evt]
fn do_module(_md: Metadata, event: DoModule) -> Result<()> {
    warn_if_unexpected(&DO_MODULE, event.id);
    Ok(())
}

fn warn_if_unexpected(expected: &Module, found: u32) {
    if let Some(warning) = unexpected_module(expected, found) {
        let _ = sdk::warn!("{warning}").ok();
    }
}

fn unexpected_module(expected: &Module, found: u32) -> Option<String> {
    (found != expected.id).then(|| {
        sdk::format!(
            "found module {found:#010x} where {} ({:#010x}) is expected: \
             the addresses in ur20-modbus-tcp-bridge.yml do not fit the coupler",
            expected.name,
            expected.id
        )
    })
}

// The coupler forgets module parameters not saved in its web server when it
// loses power, so the cell sets them again whenever it finds them otherwise.
#[sdk::evt]
fn pt100_measurement_range(_md: Metadata, event: Pt100MeasurementRange) -> Result<()> {
    if event.range != MEASUREMENT_RANGE_PT100 {
        let _ = sdk::info!("set the RTD channel to PT100, found range {}", event.range).ok();
        UR20_CLIENT.set_pt100_measurement_range(SetPt100MeasurementRange {
            range: MEASUREMENT_RANGE_PT100,
        })?;
    }
    Ok(())
}

// Filters out the bouncing of the heat button already at the coupler.
#[sdk::evt]
fn heat_button_input_delay(_md: Metadata, event: HeatButtonInputDelay) -> Result<()> {
    if event.delay != INPUT_DELAY_10_MS {
        let _ = sdk::info!(
            "set the input delay of the heat button to 10 ms, found {}",
            event.delay
        )
        .ok();
        UR20_CLIENT.set_heat_button_input_delay(SetHeatButtonInputDelay {
            delay: INPUT_DELAY_10_MS,
        })?;
    }
    Ok(())
}

#[sdk::evt]
fn pt100_connection_type(_md: Metadata, event: Pt100ConnectionType) -> Result<()> {
    if event.connection != CONNECTION_TYPE_TWO_WIRE {
        let _ = sdk::info!(
            "set the RTD channel to two-wire, found connection type {}",
            event.connection
        )
        .ok();
        UR20_CLIENT.set_pt100_connection_type(SetPt100ConnectionType {
            connection: CONNECTION_TYPE_TWO_WIRE,
        })?;
    }
    Ok(())
}

#[sdk::evt]
fn temperature(_md: Metadata, event: Temperature) -> Result<()> {
    let celsius = f32::from(event.tenths) / 10.0;
    let _ = sdk::debug!("temperature {celsius:.1} °C").ok();
    if let Some(on) = fan_switch(celsius, FAN_STATE.load()?) {
        UR20_CLIENT.fan_on(FanOn { on })?;
        FAN_STATE.save(&on)?;
        let _ = sdk::info!("fan {} at {celsius:.1} °C", if on { "on" } else { "off" }).ok();
    }
    if overheated(celsius, HEATER_STATE.load()?) {
        switch_heater(false)?;
        let _ = sdk::warn!("heater off, PT100 at {celsius:.1} °C").ok();
    }

    Ok(())
}

#[derive(Serialize, Deserialize, sdk::Message)]
struct Heat {
    on: bool,
}

#[sdk::init]
fn init(_md: Metadata) -> Result<()> {
    // Timers die with the previous incarnation, but the coupler keeps its
    // outputs across a restart of this cell.
    HEAT_TIMER.save(&None)?;
    switch_heater(false)
}

#[sdk::cmd]
fn heat(_md: Metadata, heat: Heat) -> Result<()> {
    switch_heater(heat.on)?;
    let _ = sdk::info!("heater {}", if heat.on { "on" } else { "off" }).ok();
    Ok(())
}

#[sdk::evt]
fn heat_button(_md: Metadata, event: HeatButton) -> Result<()> {
    if event.pressed {
        let on = !HEATER_STATE.load()?.unwrap_or_default();
        switch_heater(on)?;
        let _ = sdk::info!("heat button: heater {}", if on { "on" } else { "off" }).ok();
    }
    Ok(())
}

#[sdk::cmd]
fn heating_time_over(_md: Metadata) -> Result<()> {
    HEAT_TIMER.save(&None)?;
    switch_heater(false)?;
    let _ = sdk::info!("heater off after {} s", HEATING_TIME.as_secs()).ok();
    Ok(())
}

/// Switches the heater on for [`HEATING_TIME`] at most.
fn switch_heater(on: bool) -> Result<()> {
    if let Some(timer) = HEAT_TIMER.load()?.flatten() {
        timer.cancel()?;
    }
    let timer = if on {
        Some(sdk::delay(Callback::of::<heating_time_over>(), HEATING_TIME).build()?)
    } else {
        None
    };
    HEAT_TIMER.save(&timer)?;
    UR20_CLIENT.heater_on(HeaterOn { on })?;
    HEATER_STATE.save(&on)
}

fn overheated(celsius: f32, heater: Option<bool>) -> bool {
    heater == Some(true) && celsius > HEATER_OFF_ABOVE_CELSIUS
}

fn fan_switch(celsius: f32, fan: Option<bool>) -> Option<bool> {
    let on = if celsius > FAN_ON_ABOVE_CELSIUS {
        true
    } else if celsius < FAN_OFF_BELOW_CELSIUS {
        false
    } else {
        return None;
    };

    (fan != Some(on)).then_some(on)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switches_the_fan_on_above_fan_on() {
        assert_eq!(fan_switch(FAN_ON_ABOVE_CELSIUS, Some(false)), None);
        assert_eq!(
            fan_switch(FAN_ON_ABOVE_CELSIUS + 0.1, Some(false)),
            Some(true)
        );
    }

    #[test]
    fn switches_the_fan_off_below_fan_off() {
        assert_eq!(fan_switch(FAN_OFF_BELOW_CELSIUS, Some(true)), None);
        assert_eq!(
            fan_switch(FAN_OFF_BELOW_CELSIUS - 0.1, Some(true)),
            Some(false)
        );
    }

    #[test]
    fn leaves_the_fan_between_the_thresholds() {
        assert_eq!(fan_switch(27.0, Some(true)), None);
        assert_eq!(fan_switch(27.0, Some(false)), None);
    }

    #[test]
    fn leaves_a_fan_that_already_is_as_it_should_be() {
        assert_eq!(fan_switch(30.0, Some(true)), None);
        assert_eq!(fan_switch(20.0, Some(false)), None);
    }

    #[test]
    fn switches_the_heater_off_above_heater_off() {
        assert!(!overheated(HEATER_OFF_ABOVE_CELSIUS, Some(true)));
        assert!(overheated(HEATER_OFF_ABOVE_CELSIUS + 0.1, Some(true)));
    }

    #[test]
    fn leaves_a_heater_that_is_off() {
        assert!(!overheated(HEATER_OFF_ABOVE_CELSIUS + 0.1, Some(false)));
        assert!(!overheated(HEATER_OFF_ABOVE_CELSIUS + 0.1, None));
    }

    #[test]
    fn accepts_the_expected_module() {
        assert_eq!(unexpected_module(&RTD_MODULE, 0x0406_1544), None);
    }

    #[test]
    fn names_the_expected_and_the_found_module() {
        let warning = unexpected_module(&RTD_MODULE, 0x0009_1F84).unwrap();
        assert!(warning.contains("0x00091f84"), "{warning}");
        assert!(
            warning.contains("UR20-4AI-RTD-DIAG (0x04061544)"),
            "{warning}"
        );
    }

    #[test]
    fn switches_a_fan_not_switched_yet() {
        assert_eq!(fan_switch(30.0, None), Some(true));
        assert_eq!(fan_switch(20.0, None), Some(false));
    }
}
