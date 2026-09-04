//! The JSON shapes the gateway's cell API accepts: events to publish and
//! commands to send, as posted by external clients.

/// One interaction an external client requests via the gateway, tagged by
/// `type`.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum CellInteraction<'a> {
    /// Publish an event.
    Event(#[serde(borrow)] Event<'a>),
    /// Send a command to a cell.
    Command(#[serde(borrow)] Command<'a>),
}

/// An event name as sent over the gateway API.
pub type EventName = alloc::string::String;
/// A command name as sent over the gateway API.
pub type CommandName = alloc::string::String;
/// A raw payload as sent over the gateway API.
pub type Bytes = alloc::vec::Vec<u8>;

/// A request to publish an event.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[allow(missing_docs)]
pub struct Event<'a> {
    pub name: &'a str,
    /// The encoded event payload; `None` for payload-less events.
    pub payload: Option<&'a [u8]>,
}

/// A request to send a command to a cell.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[allow(missing_docs)]
pub struct Command<'a> {
    pub sri: &'a str,
    pub name: &'a str,
    /// The encoded command payload; `None` for payload-less commands.
    pub payload: Option<&'a [u8]>,
}

/// Why an interaction was rejected, mapped onto an HTTP status.
#[derive(Debug)]
#[allow(missing_docs)]
pub struct Error {
    /// The HTTP status code to answer with.
    pub status: u16,
    pub message: &'static str,
}
