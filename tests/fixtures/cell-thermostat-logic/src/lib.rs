//! Thermostat cell: parses a temperature string, forwards it to the room cell,
//! and publishes a confirmation on `room_temperature_set`.

#![no_std]

use myrmic_sdk::{Metadata, Result, Sri, String, publish, send};

const ROOM_SRI: &str = "room_cell";

#[myrmic_sdk::cmd]
fn set_room_temperature(_md: Metadata, s: String) -> Result<()> {
    let degrees: i32 = s.trim().parse().map_err(|_| "parse")?;
    let room = Sri::of_path(ROOM_SRI).map_err(|_| "invalid sri")?;
    // The room cell decodes `Temperature { degrees_celsius: i32 }`, which is
    // postcard-identical to a bare `i32`.
    send(room, "set_temperature", &degrees)?;
    publish("room_temperature_set", &s)?;
    Ok(())
}
