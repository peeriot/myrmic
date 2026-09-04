//! The task that feeds a cell's inbound command queue into its message channel.
//!
//! The mailbox mechanics (batched polling, dead-lettering, the receipt that
//! consumes a message) live in [`cell_mailbox::CommandStream`]; this only adapts
//! each command into an [`IncomingMessage`] and applies per-cell backpressure.

use std::time::Duration;

use cell_mailbox::CommandStream;
use cell_protocol::{MailboxCommand, Sri};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{AbortHandle, JoinHandle};

use crate::wasm::cell::{CellCommand, CellMessage, CommandOrigin, IncomingMessage};

/// Spawns the task that pulls commands off the cell's mailbox and forwards them
/// on `message_tx`. Serialized: it waits for each command to be fully handled
/// before offering the next, so the stream never re-reads a message that is
/// still in flight. The returned receiver fires once the command subscription is
/// live.
pub(crate) fn spawn_command_producer(
    db: &db_client::v1::Client,
    sri: Sri,
    message_tx: mpsc::Sender<IncomingMessage>,
    poll_interval: Duration,
    batch_size: usize,
) -> (DropHandle, oneshot::Receiver<()>) {
    let db = db.clone();
    let (ready_tx, ready_rx) = oneshot::channel();

    let task = tokio::spawn(async move {
        let mut stream = CommandStream::open(db, sri, poll_interval, batch_size).await;

        // Subscription is live — the cell can now be reported as ready.
        let _ = ready_tx.send(());

        while let Some(incoming) = stream.next().await {
            // The receipt travels with the command: the cell removes the message
            // inside the transaction its handler ran in, so consumption and the
            // handler's writes land together — or neither does.
            let (command, receipt) = incoming.into_parts();
            let MailboxCommand {
                cmd,
                payload,
                attachment,
            } = command;

            let (ready, handled) = oneshot::channel();
            let queued_at = std::time::Instant::now();
            let incoming = IncomingMessage {
                span_context: attachment.span_context(),
                message: CellMessage::Command(CellCommand {
                    cmd,
                    payload,
                    origin: CommandOrigin::Mailbox(receipt),
                    ready: Some(ready),
                    sender: attachment.sender(),
                }),
                queued_at: Some(queued_at),
            };

            if message_tx.send(incoming).await.is_err() {
                // The cell task is gone. The message was never consumed, so it
                // is still in the mailbox for whoever takes over.
                tracing::warn!("cell message receiver dropped");
                return;
            }

            // Backpressure: don't offer the next command until this one has been
            // handled, so the stream cannot re-read one that is in flight.
            let _ = handled.await;
            crate::wasm::cell::cell_task::metrics::record_turn(&sri, queued_at.elapsed());
        }
    });

    (DropHandle::from(task), ready_rx)
}

/// Aborts the wrapped task when dropped.
pub struct DropHandle(AbortHandle);

impl DropHandle {
    #[allow(dead_code)]
    pub fn forget(self) {
        let Self(_) = self;
    }

    pub fn abort(&self) {
        self.0.abort();
    }
}

impl Drop for DropHandle {
    fn drop(&mut self) {
        self.abort();
    }
}

impl From<AbortHandle> for DropHandle {
    fn from(value: AbortHandle) -> Self {
        Self(value)
    }
}

impl<T> From<JoinHandle<T>> for DropHandle {
    fn from(value: JoinHandle<T>) -> Self {
        Self::from(value.abort_handle())
    }
}
