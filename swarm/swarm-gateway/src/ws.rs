//! Upgraded `WebSocket`s: client frames go to the cell as [`WsEvent`]s, and
//! the cell's [`SocketCommand`]s go back to the client.

use axum::extract::ws::{Message, WebSocket};
use cell_protocol::Sri;
use futures_util::{SinkExt, StreamExt};
use myrmic_common::cells::Command;
use myrmic_common::gateway::{SessionId, Socket, WsEvent, WsMessage};
use tokio::sync::{mpsc, watch};

use crate::relay::{Relay, SocketCommand};

/// Runs one socket until the client, the cell, or the gateway closes it. The
/// cell hears [`WsEvent::Closed`] whichever it was.
pub(crate) async fn run(
    relay: Relay,
    cell: Sri,
    callback: Command,
    ctx: SessionId,
    ws: WebSocket,
    mut shutdown: watch::Receiver<bool>,
    _alive: mpsc::Sender<()>,
) {
    let socket = Socket {
        gateway: relay.sri(),
        ctx,
    };
    let mut commands = relay.open_socket(ctx);
    tracing::debug!("socket {ctx} up for {cell}");

    let notify = async |event: WsEvent| {
        if let Err(err) = relay.send(&cell, &callback, &event).await {
            tracing::warn!("socket {ctx}: {err}");
        }
    };

    notify(WsEvent::Open(socket)).await;

    let (mut sink, mut stream) = ws.split();
    loop {
        tokio::select! {
            frame = stream.next() => {
                let message = match frame {
                    Some(Ok(Message::Text(text))) => WsMessage::Text(text.as_str().to_owned()),
                    Some(Ok(Message::Binary(bytes))) => WsMessage::Binary(bytes.to_vec()),
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                    Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                };
                notify(WsEvent::Message(socket, message)).await;
            }
            command = commands.recv() => {
                let frame = match command {
                    Some(SocketCommand::Send(WsMessage::Text(text))) => Message::Text(text.into()),
                    Some(SocketCommand::Send(WsMessage::Binary(bytes))) => Message::Binary(bytes.into()),
                    Some(SocketCommand::Close) | None => {
                        let _ = sink.close().await;
                        break;
                    }
                };
                if sink.send(frame).await.is_err() {
                    break;
                }
            }
            _ = shutdown.changed() => {
                let _ = sink.close().await;
                break;
            }
        }
    }

    relay.close_socket(ctx);
    tracing::debug!("socket {ctx} down");
    notify(WsEvent::Closed(socket)).await;
}
