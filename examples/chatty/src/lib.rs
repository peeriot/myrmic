#![no_std]

extern crate alloc;

use myrmic_sdk::db::Scope;
use myrmic_sdk::db::table::Table;
use myrmic_sdk::gateway::{Socket, WsMessage};
use myrmic_sdk::{Encoder, String, Vec};
use serde::{Deserialize, Serialize};

/// Keep the messages hidden from other cells.
const SCOPE: Scope = Scope::public("chatty");

/// Connected users, keyed by [`socket_key`].
pub const USERS: Table<User, Vec<u8>> = Table::new_in("users", SCOPE);

/// Chat history, keyed by big-endian timestamp so `list()` yields it
/// oldest -> newest.
pub const MESSAGES: Table<Message, Vec<u8>> = Table::new_in("messages", SCOPE);

/// A connected chatter.
#[derive(Clone, Serialize, Deserialize)]
pub struct User {
    /// The socket to deliver messages to.
    pub socket: Socket,
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

/// The [`USERS`] key of the user connected on `socket`.
pub fn socket_key(socket: &Socket) -> Vec<u8> {
    socket.ctx().as_bytes().to_vec()
}

/// Sends `message` to the client on `socket`, as a JSON text frame.
pub fn send_to(socket: &Socket, message: &ServerMessage) -> myrmic_sdk::Result<()> {
    let json = String::from_utf8(message.to_bytes()?).map_err(|_| "message is not utf-8")?;
    socket.send(WsMessage::Text(json))
}

/// Sends a message to every connected user.
pub fn broadcast(message: &ServerMessage) -> myrmic_sdk::Result<()> {
    for user in USERS.list()? {
        let _ = send_to(&user.socket, message);
    }
    Ok(())
}

/// Tells the client on `socket` about `user`.
pub fn send_user(socket: &Socket, user: &User) {
    let _ = send_to(
        socket,
        &ServerMessage::Connect {
            id: user.id.clone(),
            name: user.name.clone(),
        },
    );
}
