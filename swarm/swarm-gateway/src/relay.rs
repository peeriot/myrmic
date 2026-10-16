//! The gateway's own identity in the swarm, and the frames cells send it.
//!
//! One SRI per gateway process. Every request and socket is delivered to cells
//! stamped with it, so their replies land in its mailbox; a single pump drains
//! that mailbox and routes each [`GatewayFrame`] to the request or socket it
//! names.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use cell_protocol::{Gen, PlacementEntry, PlacementKind, Sri};
use myrmic_common::cells::Command;
use myrmic_common::gateway::{GATEWAY_COMMAND, GatewayFrame, HttpReply, SessionId, WsMessage};
use serde::Serialize;
use sorg_common::{Mailbox, claim_placement, remove_placement};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;
use zenoh::Session;

/// How many frames one mailbox read pulls in.
const FRAME_BATCH_SIZE: usize = 16;

/// An answer to a request, with the cell that sent it.
pub(crate) struct Reply {
    pub reply: HttpReply,
    pub sender: Option<Sri>,
}

/// What a cell asked of an open socket.
pub(crate) enum SocketCommand {
    Send(WsMessage),
    Close,
}

/// The gateway's SRI, and who is waiting on frames addressed to it.
#[derive(Clone)]
pub(crate) struct Relay {
    session: Session,
    sri: Sri,
    requests: Arc<Mutex<HashMap<SessionId, oneshot::Sender<Reply>>>>,
    sockets: Arc<Mutex<HashMap<SessionId, mpsc::Sender<SocketCommand>>>>,
}

impl Relay {
    /// Registers a fresh gateway SRI as a placeholder cell, so cells can send
    /// to it — the host rejects commands addressed to unknown cells.
    pub async fn start(session: &Session) -> anyhow::Result<(Self, Gen)> {
        let sri = Sri::from_uuid(Uuid::new_v4());
        let gen_id = Gen::from_timestamp(&session.new_timestamp());
        claim_placement(
            session,
            PlacementEntry {
                sri,
                kind: PlacementKind::Placeholder,
                app: None,
                gen_id,
            },
        )
        .await
        .map_err(|err| anyhow!("{err}"))
        .with_context(|| format!("unable to register gateway {sri}"))?;

        tracing::info!("gateway registered as {sri}");

        let relay = Self {
            session: session.clone(),
            sri,
            requests: Arc::default(),
            sockets: Arc::default(),
        };
        Ok((relay, gen_id))
    }

    pub fn sri(&self) -> Sri {
        self.sri
    }

    /// Releases the gateway's placement and clears its mailbox.
    pub async fn stop(&self, gen_id: Gen) {
        let _ = remove_placement(&self.session, &self.sri, gen_id).await;
        let _ = Mailbox::new(&self.session).drain_commands(self.sri).await;
    }

    /// Sends `payload` to `command` on `target`, from this gateway.
    pub async fn send(
        &self,
        target: &Sri,
        command: &Command,
        payload: &impl Serialize,
    ) -> anyhow::Result<()> {
        let payload = postcard::to_allocvec(payload).context("unable to encode payload")?;
        Mailbox::new(&self.session)
            .send_command(target, command, Some(payload), Some(self.sri.as_uuid()))
            .await
            .map_err(|err| anyhow!("unable to send to {target}: {err}"))
    }

    /// Waits for the reply to request `ctx`. Dropping the guard stops waiting.
    pub fn expect_reply(&self, ctx: SessionId) -> (oneshot::Receiver<Reply>, Pending) {
        let (tx, rx) = oneshot::channel();
        lock(&self.requests).insert(ctx, tx);
        (
            rx,
            Pending {
                relay: self.clone(),
                ctx,
            },
        )
    }

    /// Routes frames for socket `ctx` to the returned receiver until
    /// [`close_socket`](Self::close_socket).
    pub fn open_socket(&self, ctx: SessionId) -> mpsc::Receiver<SocketCommand> {
        let (tx, rx) = mpsc::channel(256);
        lock(&self.sockets).insert(ctx, tx);
        rx
    }

    pub fn close_socket(&self, ctx: SessionId) {
        lock(&self.sockets).remove(&ctx);
    }

    /// Drains the gateway's mailbox for as long as the gateway runs.
    pub fn spawn_pump(&self) -> tokio::task::JoinHandle<()> {
        let relay = self.clone();
        tokio::spawn(async move {
            let mut commands = Mailbox::new(&relay.session)
                .commands(relay.sri, Duration::from_secs(2), FRAME_BATCH_SIZE)
                .await;

            while let Some(incoming) = commands.next().await {
                let command = incoming.command();
                let sender = command.attachment.sender().map(Sri::from_uuid);
                let frame = if command.cmd.as_ref() == GATEWAY_COMMAND {
                    postcard::from_bytes::<GatewayFrame>(command.payload.as_deref().unwrap_or(&[]))
                        .map_err(|err| anyhow!("malformed frame: {err}"))
                } else {
                    Err(anyhow!("unknown command '{}'", command.cmd.as_ref()))
                };

                // A frame that can't be consumed redelivers; route it then.
                if let Err(err) = incoming.consume().await {
                    tracing::warn!("gateway frame consume failed: {err}");
                    continue;
                }

                match frame {
                    Ok(frame) => relay.route(frame, sender),
                    Err(err) => tracing::warn!(
                        "gateway dropped a frame from {}: {err}",
                        sender.map_or_else(|| "unknown".to_owned(), |s| s.to_string())
                    ),
                }
            }
        })
    }

    fn route(&self, frame: GatewayFrame, sender: Option<Sri>) {
        match frame {
            GatewayFrame::Reply { ctx, reply } => {
                let waiter = lock(&self.requests).remove(&ctx);
                match waiter {
                    Some(waiter) => {
                        let _ = waiter.send(Reply { reply, sender });
                    }
                    None => tracing::debug!("reply for unknown or answered request {ctx}"),
                }
            }
            GatewayFrame::Send { ctx, message } => {
                self.to_socket(ctx, SocketCommand::Send(message));
            }
            GatewayFrame::Close { ctx } => self.to_socket(ctx, SocketCommand::Close),
        }
    }

    /// Never blocks the pump: a socket too slow to keep up is closed, which
    /// its task sees as its channel ending.
    fn to_socket(&self, ctx: SessionId, command: SocketCommand) {
        let mut sockets = lock(&self.sockets);
        let Some(socket) = sockets.get(&ctx) else {
            tracing::debug!("frame for unknown or closed socket {ctx}");
            return;
        };
        if let Err(mpsc::error::TrySendError::Full(_)) = socket.try_send(command) {
            tracing::warn!("socket {ctx} is not keeping up; closing it");
            sockets.remove(&ctx);
        }
    }
}

/// Stops waiting for a request's reply when dropped.
pub(crate) struct Pending {
    relay: Relay,
    ctx: SessionId,
}

impl Drop for Pending {
    fn drop(&mut self) {
        lock(&self.relay.requests).remove(&self.ctx);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
