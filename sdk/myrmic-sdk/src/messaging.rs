//! Typed messaging sugar.
//!
//! [`publish`] emits `value` as an event; [`send`] sends it as a fire-and-forget
//! command to a target [`Sri`]. Both take the message name explicitly (anything
//! `TryInto<Event>` / `TryInto<Command>`) and encode the payload via its
//! [`Encoder`] impl.

use crate::{Command, Encoder, Event, Result, Sri};

/// Publish `value` as an event under an explicit `name`.
pub fn publish<T: Encoder + ?Sized>(name: impl TryInto<Event>, value: &T) -> Result<()> {
    let evt = name.try_into().map_err(|_| "invalid event name")?;
    let payload = value.to_bytes()?;

    let req = myrmic_common::cells::EventPublishRequest {
        event: evt,
        payload: Some(payload),
    };

    crate::host_functions::publish_event(&req)?;

    Ok(())
}

/// Send `value` as a fire-and-forget command to `sri` under an explicit `name`.
pub fn send<T: Encoder + ?Sized>(sri: Sri, name: impl TryInto<Command>, value: &T) -> Result<()> {
    let command = name.try_into().map_err(|_| "invalid command name")?;
    let payload = value.to_bytes()?;

    let req = myrmic_common::cells::CommandRequest {
        sri,
        command,
        payload: Some(payload),
    };

    crate::host_functions::send_command(&req).map_err(|err| err.describe())
}
