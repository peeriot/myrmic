//! Replies from a bridge to the cell that sent it a command.
//!
//! Commands are fire-and-forget. A generated client that expects an answer names a
//! callback command in the reserved `__callback` field of its payload; the bridge
//! answers by sending that command back to the calling cell, with the reply as its
//! payload.

use cell_protocol::Sri;
use myrmic_common::cells::Command;
use sorg_common::{OutgoingMessage, custom_err};
use uuid::Uuid;

/// The reserved payload field a generated client carries its callback name in.
const CALLBACK_FIELD: &str = "__callback";

/// Removes the callback name from a command payload, so it is not mistaken for a
/// value field. `None` if the caller wants no reply.
pub(crate) fn take_callback(
    obj: &mut serde_json::Map<String, serde_json::Value>,
) -> crate::Result<Option<String>> {
    match obj.remove(CALLBACK_FIELD) {
        Some(serde_json::Value::String(name)) => Ok(Some(name)),
        Some(_) => Err(custom_err!("`{CALLBACK_FIELD}` must be a string"))?,
        None => Ok(None),
    }
}

/// Sends `reply` to the command `callback` of the cell `sender`, as coming from
/// the bridge cell `bridge_cell_name`.
pub(crate) async fn deliver(
    db: &db_client::v1::Client,
    bridge_cell_name: &str,
    sender: Uuid,
    callback: String,
    reply: Vec<u8>,
) -> crate::Result<()> {
    let command =
        Command::new(callback).map_err(|err| custom_err!("invalid callback name: {}", err))?;
    let mut message = OutgoingMessage::command(&Sri::from_uuid(sender), &command, Some(reply))
        .map_err(|err| custom_err!("unable to build reply command: {}", err))?;
    if let Ok(bridge) = Sri::from_target(bridge_cell_name) {
        message.attach_sender(Some(bridge.as_uuid()));
    }
    message
        .send_via_db(db, None)
        .await
        .map_err(|err| custom_err!("unable to deliver reply: {}", err))?;

    tracing::debug!("delivered reply to callback `{}`", command.as_ref());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        match value {
            serde_json::Value::Object(obj) => obj,
            other => panic!("expected an object, got {other}"),
        }
    }

    #[test]
    fn takes_the_callback_out_of_the_payload() {
        let mut obj = object(serde_json::json!({"__callback": "fetched", "id": 1}));

        assert_eq!(take_callback(&mut obj).unwrap(), Some("fetched".to_owned()));
        assert_eq!(obj, object(serde_json::json!({"id": 1})));
    }

    #[test]
    fn a_payload_without_callback_wants_no_reply() {
        let mut obj = object(serde_json::json!({"id": 1}));
        assert_eq!(take_callback(&mut obj).unwrap(), None);
    }

    #[test]
    fn a_callback_must_be_a_string() {
        let mut obj = object(serde_json::json!({"__callback": 7}));
        assert!(take_callback(&mut obj).is_err());
    }
}
