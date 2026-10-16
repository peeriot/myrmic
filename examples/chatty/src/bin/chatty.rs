#![no_std]
#![no_main]

use chatty::{ServerMessage, USERS, User, send_to, send_user, socket_key};
use myrmic_sdk::gateway::{self, Asset, HttpRequest, Socket, WsEvent, WsMessage};
use myrmic_sdk::{Callback, Decoder, Metadata};
use textus::Embed;

#[derive(textus::Embed)]
#[embed(path = "./dist")]
struct Assets;

#[myrmic_sdk::init]
fn init(md: Metadata) -> myrmic_sdk::Result {
    gateway::assets(md.id).upload(Assets::iter())?;
    gateway::mount("/chat", Callback::of::<on_http>())?;

    let _ = myrmic_sdk::info!("chatty server up on /chat (id={:?})", md.id).ok();
    Ok(())
}

/// `/ws` is the chat socket; everything else is the web client.
#[myrmic_sdk::cmd]
fn on_http(_md: Metadata, req: HttpRequest) -> myrmic_sdk::Result {
    if req.path == "/ws" {
        return req.upgrade(Callback::of::<on_ws>());
    }

    let asset = Asset::new(req.path.clone()).index("/index.html");
    req.asset(asset)
}

#[myrmic_sdk::cmd]
fn on_ws(_md: Metadata, event: WsEvent) -> myrmic_sdk::Result {
    match event {
        // Users announce themselves with a `connect` message.
        WsEvent::Open(_) => Ok(()),
        WsEvent::Message(socket, WsMessage::Text(text)) => {
            let msg = ServerMessage::from_bytes(text.into_bytes())?;
            recv_message(socket, msg)
        }
        WsEvent::Message(_, WsMessage::Binary(_)) => Err("chatty only speaks text frames"),
        WsEvent::Closed(socket) => {
            let key = socket_key(&socket);
            let Some(user) = USERS.get(&key)? else {
                return Ok(());
            };
            USERS.delete(&key)?;
            chatty::broadcast(&ServerMessage::Disconnect { id: user.id })
        }
    }
}

fn recv_message(socket: Socket, msg: ServerMessage) -> myrmic_sdk::Result {
    match msg {
        // A user joined: send them the current roster, register them, tell
        // everyone else, and replay history.
        ServerMessage::Connect { id, name } => {
            let new_user = User { socket, id, name };

            for user in USERS.list()? {
                // Tell the connecting user about every existing user...
                send_user(&socket, &user);
                // ...and every existing user about the connecting one.
                send_user(&user.socket, &new_user);
            }

            // Insert it into the db
            // (Remember, everything is done in a single atomic transaction, so order is less important)
            USERS.insert_with(&socket_key(&socket), &new_user)?;

            // And now we catch them up with the last 100 messages.
            let history = chatty::recent_messages(100);
            if !history.is_empty() {
                send_to(&socket, &ServerMessage::Chat { messages: history })?;
            }
        }
        // A chat message: store it and fan it out to everyone (incl. sender).
        ServerMessage::Chat { messages } => {
            chatty::store_messages(&messages);
            chatty::broadcast(&ServerMessage::Chat { messages })?;
        }
        // A user left: drop them and tell everyone else.
        ServerMessage::Disconnect { id } => {
            USERS.delete(&socket_key(&socket))?;
            chatty::broadcast(&ServerMessage::Disconnect { id })?;
        }
    }

    Ok(())
}
