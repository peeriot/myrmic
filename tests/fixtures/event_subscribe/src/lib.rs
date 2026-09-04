//! Subscribes to `my_event` — the runtime auto-subscribes to the `event_my_event`
//! export — and forwards the payload to `other_event`. It also republishes the event
//! sender's SRI (raw 16 bytes) on `evt_sender`, so a HIL test can verify that an embedded
//! subscriber receives the publisher's identity.
#![no_std]

use myrmic_sdk::{Bytes, EventPublishRequest, Metadata, Result, publish_event};

#[myrmic_sdk::evt]
fn my_event(md: Metadata, payload: Bytes) -> Result<()> {
    publish_event(&EventPublishRequest {
        event: "evt_sender".try_into()?,
        payload: Some(md.sender.to_bytes().to_vec()),
    })?;
    publish_event(&EventPublishRequest {
        event: "other_event".try_into()?,
        payload: Some(payload),
    })?;
    Ok(())
}
