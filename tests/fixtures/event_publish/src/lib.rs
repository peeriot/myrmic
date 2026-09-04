//! Publishes a fixed payload on `my_event` when its `publish_event` command runs.
#![no_std]

use myrmic_sdk::{EventPublishRequest, Metadata, Result};

#[myrmic_sdk::cmd]
fn publish_event(_md: Metadata) -> Result<()> {
    myrmic_sdk::publish_event(&EventPublishRequest {
        event: "my_event".try_into()?,
        payload: Some(b"pub_payload".to_vec()),
    })?;
    Ok(())
}
