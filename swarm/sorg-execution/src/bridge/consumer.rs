//! Shared command-consumer loop for the MQTT/HTTP bridges.
//!
//! A bridge is a non-Wasm consumer of a cell's command mailbox: it drains
//! commands and translates each into an outbound protocol request. Unlike a
//! real cell it runs no Wasm and no transaction of its own — commands are
//! fire-and-forget, so each is consumed once its handler returns, whatever the
//! outcome.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use cell_mailbox::CommandStream;
use cell_protocol::{MailboxCommand, Sri};
use tokio::sync::{Barrier, Notify};

use crate::wasm::cell::state::DropHandle;

/// How many commands one mailbox read pulls in.
const BATCH_SIZE: usize = 16;

/// Spawns a task that, once released by the run barrier, drains `sri`'s command
/// mailbox and runs `handle` for each command. The two barrier waits mirror the
/// bridge's init/run handshake (spawned during `init`, released during `run`).
/// If the stream ever ends the `kill_signal` is fired so the bridge tears down.
pub(crate) fn spawn_bridge_command_consumer<F, Fut>(
    db: &db_client::v1::Client,
    barrier: Arc<Barrier>,
    kill_signal: Arc<Notify>,
    sri: Sri,
    poll_interval: Duration,
    mut handle: F,
) -> DropHandle
where
    F: FnMut(MailboxCommand) -> Fut + Send + 'static,
    Fut: Future<Output = crate::Result<()>> + Send,
{
    let db = db.clone();

    let task = tokio::spawn(async move {
        // Startup sync (checked in `init`), then the run gate (released in `run`).
        let _ = barrier.wait().await;
        let _ = barrier.wait().await;

        let mut stream = CommandStream::open(db, sri, poll_interval, BATCH_SIZE).await;

        loop {
            let Some(incoming) = stream.next().await else {
                kill_signal.notify_one();
                break;
            };

            let command = incoming.command().clone();
            if let Err(err) = handle(command).await {
                tracing::error!("unable to process bridge command: {err}");
            }

            // Fire-and-forget: consume the command regardless of handler outcome.
            if let Err(err) = incoming.consume().await {
                tracing::error!("unable to consume bridge command: {err}");
            }
        }
    });

    DropHandle::from(task)
}
