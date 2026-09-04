//! The gateway's web API wire types.
//!
//! A fresh, owned interaction envelope for the socket gateway's cell API. It is
//! shaped after — but independent of — the cell command/event model
//! (`myrmic_common::types::web::cells::CellInteraction`), which is borrowed,
//! and it deliberately does not build on the `Incoming`/`Outgoing` protocol
//! (that is slated for removal).
//!
//! Payloads are opaque bytes, base64-encoded on the JSON wire, so the gateway
//! stays transport-agnostic — the client and the target cell agree on the
//! payload encoding; the gateway only routes.

use serde::{Deserialize, Serialize};

/// A message from a web client into the gateway.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Send a fire-and-forget command to a cell.
    Command {
        /// Target cell — an SRN (e.g. `myapp/worker`) or a UUID SRI. Omit it to
        /// address the cell that owns the route the message arrived on, which
        /// is what a single-cell front end wants.
        #[serde(default)]
        sri: Option<String>,
        /// Command name.
        name: String,
        /// Opaque command payload (base64 in JSON).
        #[serde(default, with = "b64")]
        payload: Vec<u8>,
    },
    /// Publish an event into the network.
    Event {
        /// Event name.
        name: String,
        /// Opaque event payload (base64 in JSON).
        #[serde(default, with = "b64")]
        payload: Vec<u8>,
    },
}

/// A message from the gateway out to a web client.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Sent once when a session is established; carries the session's SRI so
    /// the client knows the identity cells will address replies to.
    Ready {
        /// The session's SRI.
        session: String,
    },
    /// A command a cell sent back to this session (a "reply").
    Command {
        /// The cell that sent it (its SRI), when recorded.
        #[serde(skip_serializing_if = "Option::is_none")]
        sri: Option<String>,
        /// Command name.
        name: String,
        /// Opaque payload (base64 in JSON).
        #[serde(with = "b64")]
        payload: Vec<u8>,
    },
    /// A dispatch or protocol error.
    Error {
        /// Human-readable error message.
        message: String,
    },
}

/// serde helper: bytes as a base64 string on the wire.
mod b64 {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize as _, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        STANDARD
            .decode(encoded.as_bytes())
            .map_err(serde::de::Error::custom)
    }
}
