//! Second subscriber: forwards `my_event` payloads to `third_event` (fan-out test).
#![no_std]

use myrmic_sdk::{Bytes, EventPublishRequest, Metadata, Result, publish_event};

#[myrmic_sdk::evt]
fn my_event(_md: Metadata, payload: Bytes) -> Result<()> {
    publish_event(&EventPublishRequest {
        event: "third_event".try_into()?,
        payload: Some(payload),
    })?;
    Ok(())
}
