//! Example cell for testing nested domain types (caller side).
//!
//! `read_value` requests a reading from the nested cell; `on_reading` publishes
//! the nested measurement value on `measurement_value`.

#![no_std]

use myrmic_sdk::{Callback, Metadata, Result, Sri, publish, send};

/// A single measurement with a value and unit identifier.
#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
#[codec(myrmic_sdk::Postcard)]
struct Measurement {
    value: i32,
    unit: i32,
}

/// A timestamped reading containing a measurement.
#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
#[codec(myrmic_sdk::Postcard)]
struct Reading {
    measurement: Measurement,
    timestamp: u64,
}

const NESTED_SRI: &str = "nested_cell";

#[myrmic_sdk::cmd]
fn read_value(_md: Metadata) -> Result<()> {
    let nested = Sri::of_path(NESTED_SRI).map_err(|_| "invalid sri")?;
    send(nested, "get_reading", &Callback::of::<on_reading>())?;
    Ok(())
}

#[myrmic_sdk::cmd]
fn on_reading(_md: Metadata, reading: Reading) -> Result<()> {
    let value = myrmic_sdk::format!("{}", reading.measurement.value);
    publish("measurement_value", &value)?;
    Ok(())
}
