#![no_std]
#![no_main]

use chatty::{ServerMessage, USERS, User, send_user};
use myrmic_sdk::{Metadata, gateway, send};
use textus::Embed;

#[derive(textus::Embed)]
#[embed(path = "./dist")]
struct Assets;

#[myrmic_sdk::init]
fn init(md: Metadata) -> myrmic_sdk::Result {
    gateway::assets(md.id).upload(Assets::iter())?;

    gateway::mount("/chat")
        .api("/api")
        .ws("/ws")
        .index("/index.html")
        .bind()
        .map_err(<&'static str>::from)?;

    let _ = myrmic_sdk::info!("chatty server up on /chat (id={:?})", md.id).ok();
    Ok(())
}

#[myrmic_sdk::cmd]
fn recv_message(md: Metadata, msg: ServerMessage) -> myrmic_sdk::Result {
    // `md.sender` is the web client's per-session SRI (stamped by the gateway),
    // i.e. the address we deliver its replies to.
    let sender = md.sender;

    match msg {
        // A user joined: send them the current roster, register them, tell
        // everyone else, and replay history.
        ServerMessage::Connect { id, name } => {
            let new_user = User {
                sri: sender,
                id: id.clone(),
                name: name.clone(),
            };

            for user in USERS.iter() {
                let (user_sri, user) = user?;

                // Now we send an update to every _other_ connected user, informing them of the new connection.
                send_user(sender, &user);

                // Need to tell the connecting user all of the existing users.
                send_user(user_sri, &new_user);
            }

            // Insert it into the db
            // (Remember, everything is done in a single atomic transaction, so order is less important)
            USERS.insert_with(&sender, &new_user)?;

            // And now we catch them up with the last 100 messages.
            let history = chatty::recent_messages(100);
            if !history.is_empty() {
                send(sender, "msg", &ServerMessage::Chat { messages: history })?;
            }
        }
        // A chat message: store it and fan it out to everyone (incl. sender).
        ServerMessage::Chat { messages } => {
            // Store the new messages
            chatty::store_messages(&messages);
            // And send the messages to everyone else.
            chatty::broadcast(&ServerMessage::Chat { messages })?;
        }
        // A user left: drop them and tell everyone else.
        ServerMessage::Disconnect { id } => {
            // Delete the user from the DB.
            USERS.delete(&sender)?;
            // And send a disconnect to everyone.
            chatty::broadcast(&ServerMessage::Disconnect { id })?;
        }
    }

    Ok(())
}
