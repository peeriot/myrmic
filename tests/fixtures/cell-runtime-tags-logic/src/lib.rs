//! Runtime-tags cell: reads the host runtime's id and effective tag set and
//! publishes them on the `runtime_report` event so a test can observe what a
//! cell sees of the runtime it landed on. The payload is `"<id>|<tag>,<tag>,…"`.
#![no_std]

use myrmic_sdk::{
    EventPublishRequest, Metadata, Result, format, publish_event, runtime_id, runtime_tags,
};

/// Publishes the runtime id and effective tags on the `runtime_report` event.
#[myrmic_sdk::cmd]
fn report_runtime(_md: Metadata) -> Result<()> {
    let id = runtime_id()?;
    let tags = runtime_tags()?;
    let payload = format!("{id}|{}", tags.join(","));
    publish_event(&EventPublishRequest {
        event: "runtime_report".try_into()?,
        payload: Some(payload.into_bytes()),
    })?;

    Ok(())
}
