//! The entries of the `myrmic telemetry debug` stream: the commands and events stored in the
//! cells' mailboxes. `--json` prints them in exactly this serde shape.

use std::fmt::Display;
use std::time::SystemTime;

use myrmic_common::cells::{Command, Event, Sri};
use uuid::Uuid;

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum DebugItem {
    Command(DebugCommand),
    Event(DebugEvent),
}

impl DebugItem {
    pub fn timestamp(&self) -> &SystemTime {
        match self {
            DebugItem::Command(debug_command) => &debug_command.inserted_at,
            DebugItem::Event(debug_event) => &debug_event.inserted_at,
        }
    }

    pub fn filter_sri(&self, sri_filter: Option<&str>) -> bool {
        match (self, sri_filter) {
            (_, None) => true,
            (DebugItem::Command(debug_command), Some(filter)) => {
                debug_command.receiver_sri.to_string() == filter
            }
            (DebugItem::Event(_debug_event), Some(_)) => false,
        }
    }
}

impl Display for DebugItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DebugItem::Command(debug_command) => write!(f, "{debug_command}"),
            DebugItem::Event(debug_event) => write!(f, "{debug_event}"),
        }
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct DebugCommand {
    pub trace_id: Option<Uuid>,
    pub inserted_at: SystemTime,
    pub receiver_sri: Sri,
    pub cmd: Command,
    pub payload: Option<DebugPayload>,
}

impl Display for DebugCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let trace_block = match &self.trace_id {
            Some(trace_id) => format!(", trace_id={trace_id}"),
            None => String::new(),
        };
        let payload_block = match &self.payload {
            Some(payload) => format!(", payload={payload}"),
            None => String::new(),
        };

        write!(
            f,
            "[{}] COMMAND={} receiver_sri={}{}{}",
            humantime::format_rfc3339(self.inserted_at),
            self.cmd.as_ref(),
            self.receiver_sri,
            trace_block,
            payload_block
        )
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct DebugEvent {
    pub trace_id: Option<Uuid>,
    pub inserted_at: SystemTime,
    pub event_name: Event,
    pub payload: DebugPayload,
}

impl Display for DebugEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let trace_block = match self.trace_id {
            Some(trace_id) => format!(", trace_id={trace_id}"),
            None => String::new(),
        };
        write!(
            f,
            "[{}] EVENT={} payload={}{}",
            humantime::format_rfc3339(self.inserted_at),
            self.event_name.as_ref(),
            self.payload,
            trace_block,
        )
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DebugPayload {
    Json(serde_json::Value),
    String(String),
    Bytes(#[serde(serialize_with = "serialize_hex", deserialize_with = "deserialize_hex")] Vec<u8>),
}

fn serialize_hex<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let byte_blob = format!("0x{}", hex::encode(bytes));
    serializer.serialize_str(&byte_blob)
}

fn deserialize_hex<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let byte_blob: String = serde::Deserialize::deserialize(deserializer)?;
    let digits = byte_blob
        .strip_prefix("0x")
        .ok_or_else(|| serde::de::Error::custom("hex payload without a 0x prefix"))?;
    hex::decode(digits).map_err(serde::de::Error::custom)
}

impl DebugPayload {
    pub fn new(bytes: Vec<u8>) -> Self {
        // let's try to parse the payloadas JSON first
        if let Ok(json) = serde_json::from_slice(&bytes) {
            return Self::Json(json);
        }

        // next helpful debug interpretation would be String
        if let Ok((s, remainder)) = postcard::take_from_bytes::<String>(&bytes)
            && remainder.is_empty()
        {
            return Self::String(s);
        }

        // everything else is presented as bytes
        if let Ok(s) = String::from_utf8(bytes.clone()) {
            return Self::String(s);
        }

        Self::Bytes(bytes)
    }
}

impl Display for DebugPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DebugPayload::Json(value) => {
                if let Ok(s) = serde_json::to_string(value) {
                    write!(f, "{s}")
                } else {
                    write!(f, "invalid JSON")
                }
            }
            DebugPayload::String(s) => write!(f, "{s:?}"),
            DebugPayload::Bytes(items) => {
                write!(f, "0x{}", hex::encode(items))
            }
        }
    }
}
