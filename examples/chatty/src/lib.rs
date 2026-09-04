#![no_std]

extern crate alloc;

use serde::{Deserialize, Serialize};
use myrmic_sdk::db::Scope;
use myrmic_sdk::db::table::Table;
use myrmic_sdk::{Sri, String, Vec, send};

/// Keep the messages hidden from other cells.
const SCOPE: Scope = Scope::public("chatty");

/// Connected users, keyed by their SRI.
pub const USERS: Table<User, Sri> = Table::new_in("users", SCOPE);

/// Chat history, keyed by big-endian timestamp so `list()` yields it
/// oldest -> newest.
pub const MESSAGES: Table<Message, Vec<u8>> = Table::new_in("messages", SCOPE);

/// A connected chatter.
#[derive(Clone, Serialize, Deserialize)]
pub struct User {
    /// The session SRI to deliver messages to.
    pub sri: Sri,
    /// The chatter's stable id.
    pub id: String,
    /// Display name.
    pub name: String,
}

/// A single chat message. Field names match the web client's `Message` type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Sender id.
    pub sender: String,
    /// ms epoch.
    pub timestamp: u64,
    /// Message text.
    pub text: String,
}

/// Messages exchanged with the web client. JSON-encoded (the `Message` derive
/// defaults to JSON), tagged to match the client's `ServerMessage` union.
#[derive(Debug, Clone, Serialize, Deserialize, myrmic_sdk::Message)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServerMessage {
    /// A user joined (or renamed).
    Connect { id: String, name: String },
    /// A user left.
    Disconnect { id: String },
    /// One or more chat messages.
    Chat { messages: Vec<Message> },
}

/// Appends chat messages to the history.
pub fn store_messages(messages: &[Message]) {
    for message in messages {
        // Big-endian so the key sorts chronologically.
        let key = message.timestamp.to_be_bytes().to_vec();
        let _ = MESSAGES.insert_with(&key, message);
    }
}

/// The most recent N messages, oldest -> newest.
///
/// Fetches only the newest rows via the table Ordering, without loading them all.
pub fn recent_messages(limit: usize) -> Vec<Message> {
    let mut recent: Vec<Message> = MESSAGES
        .iter_rev()
        .take(limit)
        .filter_map(|row| row.ok())
        .map(|(_key, message)| message)
        .collect();
    recent.reverse();
    recent
}

/// Broadcasts a message to all users except the one passed in.
pub fn broadcast(message: &ServerMessage) -> myrmic_sdk::Result<()> {
    for user in USERS.list()? {
        let _ = send(user.sri, "msg", message);
    }
    Ok(())
}

pub fn send_user(sri: Sri, user: &User) {
    let _ = send(
        sri,
        "msg",
        &ServerMessage::Connect {
            id: user.id.clone(),
            name: user.name.clone(),
        },
    );
}
