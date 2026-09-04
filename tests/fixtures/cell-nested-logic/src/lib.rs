//! Example cell for testing nested domain types.
//!
//! `Reading` embeds a `Measurement`. `get_reading` replies to the caller's
//! callback with a sample reading.

#![no_std]

use myrmic_sdk::{Callback, Metadata, Result};

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

#[myrmic_sdk::cmd]
fn get_reading(md: Metadata, cb: Callback<Reading>) -> Result<()> {
    cb.invoke(
        md.sender,
        &Reading {
            measurement: Measurement { value: 42, unit: 1 },
            timestamp: 1000,
        },
    )?;
    Ok(())
}
