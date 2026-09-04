//! Self-contained cell messaging: one crate that owns both directions
//! (sending and receiving) of a cell's command and event traffic over the db.
//!
//! [`Mailbox`] is the single entry point. Sending builds an [`OutgoingMessage`]
//! and inserts it into the right table; receiving hands back a [`CommandStream`]
//! (a per-cell queue, drained by consuming each command) or an [`EventStream`]
//! (a cursored public log). Both receive paths batch-read from the same
//! [`PolledTable`] primitive: change-driven wakeups with a periodic backstop.
//!
//! The crate depends only on `db-client` and `cell-protocol` (no `sorg-common`,
//! no `wasmtime`), so the gateway and clients can use it without pulling in the
//! execution runtime.

mod command;
mod error;
mod event;
mod outgoing;

use std::time::Duration;

use cell_protocol::{MailboxCommand, Sri};
use db_client::Session;
use db_client::v1::Client;
use db_client::v1::models::TxId;
use myrmic_common::cells::{Command, Event};
use uuid::Uuid;

pub use command::{CommandReceipt, CommandStream, IncomingCommand, SettleHandle, Settlement};
pub use db_client::PolledTable;
pub use error::{Error, Result};
pub use event::EventStream;
pub use outgoing::OutgoingMessage;

/// A handle to a cell's message transport — send and receive, commands and
/// events — backed by a single db client.
///
/// Cheap to clone-construct; holds only a db client. Senders that need to
/// attach a trace span or join an existing transaction build an
/// [`OutgoingMessage`] directly and pass it to [`send`](Self::send).
#[derive(Clone)]
pub struct Mailbox {
    db: Client,
}

impl Mailbox {
    /// Build a mailbox from a session.
    pub fn new(session: &Session) -> Self {
        Self {
            db: Client::new(session),
        }
    }

    /// Build a mailbox from an existing db client.
    pub fn from_db(db: Client) -> Self {
        Self { db }
    }

    /// The underlying db client.
    pub fn db(&self) -> &Client {
        &self.db
    }

    /// Send a pre-built (and pre-decorated) message. Joins `tx` if provided,
    /// otherwise opens and commits its own transaction.
    pub async fn send(&self, message: OutgoingMessage, tx: Option<TxId>) -> Result<()> {
        message.send_via_db(&self.db, tx).await
    }

    /// Fire-and-forget a command to `sri`, stamped with `sender` (the emitting
    /// identity, or `None` for external origins like a CLI or gateway session).
    pub async fn send_command(
        &self,
        sri: &Sri,
        command: &Command,
        payload: Option<Vec<u8>>,
        sender: Option<Uuid>,
    ) -> Result<()> {
        let mut message = OutgoingMessage::command(sri, command, payload)?;
        message.attach_sender(sender);
        message.send_via_db(&self.db, None).await
    }

    /// Publish an event, stamped with `sender`.
    pub async fn publish_event(
        &self,
        event: &Event,
        payload: Option<Vec<u8>>,
        sender: Option<Uuid>,
    ) -> Result<()> {
        let mut message = OutgoingMessage::event(event, payload)?;
        message.attach_sender(sender);
        message.send_via_db(&self.db, None).await
    }

    /// Open the command queue for `sri`, reading up to `batch_size` messages at
    /// a time.
    pub async fn commands(
        &self,
        sri: Sri,
        poll_interval: Duration,
        batch_size: usize,
    ) -> CommandStream {
        CommandStream::open(self.db.clone(), sri, poll_interval, batch_size).await
    }

    /// Subscribe to the cursored event log for `event`.
    pub async fn events(&self, event: Event) -> Result<EventStream> {
        EventStream::subscribe(self.db.clone(), event).await
    }

    /// Read and remove every command currently queued for `sri` in one
    /// transaction, returning the decoded commands. For one-shot clears; use
    /// [`commands`](Self::commands) for ongoing consumption.
    pub async fn drain_commands(&self, sri: Sri) -> Result<Vec<MailboxCommand>> {
        command::drain_commands(&self.db, sri).await
    }
}
