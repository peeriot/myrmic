use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec::Vec;

use cell_protocol::{
    EVENTS_TABLE, MESSAGES_TABLE, MailboxCommand, MailboxEvent, Sri, scope_of_cell, scope_of_event,
};
use db_client::v1::Client;
use db_client::v1::models as db_models;
use db_client::v1::models::{Cursor, Id};
use myrmic_common::cells::Event;
use wasm_runtime::async_request::CommandHandledGuard;
use wasm_runtime::{CellMessage, CommandOrigin};
use wasm_storage::__reexports::postcard;

pub(crate) enum MailboxItem {
    Command {
        dest_sri: Sri,
        command: MailboxCommand,
    },
    Event(MailboxEvent),
}

impl MailboxItem {
    /// The scope this item is published into.
    fn scope(&self) -> db_models::Scope {
        match self {
            MailboxItem::Command { dest_sri, .. } => scope_of_cell(*dest_sri),
            MailboxItem::Event(ev) => scope_of_event(ev.event.as_ref()),
        }
    }

    /// The (scope, table, encoded value) triple this item is stored as.
    fn into_row(self) -> Result<(db_models::Scope, String, Vec<u8>), ()> {
        match self {
            MailboxItem::Command { dest_sri, command } => Ok((
                scope_of_cell(dest_sri),
                String::from(MESSAGES_TABLE),
                postcard::to_allocvec(&command).map_err(|_err| ())?,
            )),
            MailboxItem::Event(ev) => Ok((
                scope_of_event(ev.event.as_ref()),
                EVENTS_TABLE.to_owned(),
                postcard::to_allocvec(&ev).map_err(|_err| ())?,
            )),
        }
    }

    /// The write that delivers this item. Nobody reads the row id back, so it
    /// is an append — deferrable, and free until the application it joins
    /// flushes.
    pub(crate) fn into_append_op(self) -> Result<db_models::tb_append::Op, ()> {
        let (scope, table, value) = self.into_row()?;

        Ok(db_models::tb_append::Op {
            scope,
            table,
            eid: None,
            value,
        })
    }
}

/// Queries _and_ removes an item if possible from the given table. If no entries were found, then
/// None will be returned.
pub(crate) async fn poll_table_then_delete(
    db: &Client,
    scope: &db_models::Scope,
    table: &str,
    last_msg_id: Option<db_models::Id>,
) -> Option<(db_models::TxId, db_models::Id, Vec<u8>)> {
    let req = db_models::tx_begin::Request::routed(scope.clone());

    let tx_id = match db.send(req).await {
        Ok(Ok(response)) => response.id,
        Ok(Err(err)) => {
            log::error!(
                "unable to start db transaction [db encountered an issue] {}",
                err.message
            );
            return None;
        }
        Err(err) => {
            log::error!(
                "failed to begin mailbox transaction [unable to communicate over zenoh]: {}",
                err
            );
            return None;
        }
    };

    let cursor = last_msg_id.map(db_models::Cursor::After);

    let list_req = db_models::tb_list::Request {
        id: tx_id,
        op: db_models::tb_list::Op {
            scope: scope.clone(),
            table: String::from(table),
            cursor,
            limit: Some(1),
            order: None,
        },
    };

    let mut entities = match db.send(list_req).await {
        Ok(Ok(response)) => response.entities,
        Ok(Err(err)) => {
            log::error!("failed to poll mailbox for cell {table}: {}", err.message);
            drop(db.send(db_models::tx_rollback::Request { id: tx_id }).await);
            return None;
        }
        Err(err) => {
            log::error!(
                "unable to list entities [unable to communicate over zenoh]: {}",
                err
            );
            return None;
        }
    };

    let Some((msg_id, value)) = entities.pop() else {
        drop(db.send(db_models::tx_rollback::Request { id: tx_id }).await);
        return None;
    };

    {
        let req = db_models::tb_delete::Request {
            id: tx_id,
            op: db_models::tb_delete::Op {
                scope: scope.clone(),
                table: String::from(table),
                eid: msg_id.clone(),
            },
        };

        match db.send(req).await {
            Ok(Ok(_)) => (),
            Ok(Err(err)) => {
                log::error!("failed to delete mailbox message: {}", err.message);
                drop(db.send(db_models::tx_rollback::Request { id: tx_id }).await);
                return None;
            }
            Err(err) => {
                log::error!(
                    "unable to delete message [unable to communicate over zenoh]: {}",
                    err,
                );
                drop(db.send(db_models::tx_rollback::Request { id: tx_id }).await);
                return None;
            }
        }
    }

    let output = (tx_id, msg_id, value);

    Some(output)
}

/// Reads the command at the head of the cell's mailbox without removing it.
///
/// The message stays queued until the runtime removes it inside the transaction of the handler that
/// ran it, so a handler that fails leaves its command to be delivered again. No cursor is kept, so
/// nothing is ever skipped.
///
/// Undecodable messages are dropped as they are met — they can never be handled, so leaving one
/// would block the head of the queue forever.
pub(crate) async fn next_command(client: &Client, sri: &Sri) -> Option<CellMessage> {
    loop {
        let (msg_id, payload) = read_head(client, sri).await?;

        let Ok(command) = postcard::from_bytes::<MailboxCommand>(payload.as_slice()) else {
            log::error!(
                "[db-client] Failed to deserialize MailboxCommand {payload:?}; dropping it"
            );
            // Removing it is what lets this loop move on to the next message.
            discard_message(client, sri, msg_id).await;
            continue;
        };

        let MailboxCommand {
            cmd,
            payload,
            attachment,
        } = command;

        return Some(CellMessage::Command {
            command: cmd,
            payload,
            sender: attachment.sender().map(Sri::from_uuid),
            origin: CommandOrigin::Mailbox {
                msg_id,
                handled: CommandHandledGuard,
            },
        });
    }
}

/// Reads the head of the mailbox, leaving it in place.
async fn read_head(client: &Client, sri: &Sri) -> Option<(Id, Vec<u8>)> {
    let scope = scope_of_cell(*sri);

    let req = db_models::tx_begin::Request::routed(scope.clone());
    let tx_id = match client.send(req).await {
        Ok(Ok(response)) => response.id,
        Ok(Err(err)) => {
            log::error!("unable to start db transaction [db issue] {}", err.message);
            return None;
        }
        Err(err) => {
            log::error!("failed to begin mailbox transaction [zenoh issue]: {err}");
            return None;
        }
    };

    let listed = client
        .send(db_models::tb_list::Request {
            id: tx_id,
            op: db_models::tb_list::Op {
                scope,
                table: String::from(MESSAGES_TABLE),
                cursor: None,
                limit: Some(1),
                order: None,
            },
        })
        .await;

    // Read-only: take a clean cut and roll it back.
    drop(
        client
            .send(db_models::tx_rollback::Request { id: tx_id })
            .await,
    );

    match listed {
        Ok(Ok(response)) => response.entities.into_iter().next(),
        Ok(Err(err)) => {
            log::error!("failed to poll the mailbox: {}", err.message);
            None
        }
        Err(err) => {
            log::error!("unable to list mailbox entries [zenoh issue]: {err}");
            None
        }
    }
}

/// Removes a message in a transaction of its own — for one the runtime will
/// never be given, and for one a native cell has finished with (it has no
/// batched cell transaction to fold the removal into).
pub(crate) async fn discard_message(client: &Client, sri: &Sri, msg_id: Id) {
    let scope = scope_of_cell(*sri);

    let result = client
        .write_tx_in(scope.clone(), async move |client, tx_id| {
            client
                .send(db_models::tb_delete::Request {
                    id: tx_id,
                    op: db_models::tb_delete::Op {
                        scope,
                        table: String::from(MESSAGES_TABLE),
                        eid: msg_id,
                    },
                })
                .await
        })
        .await;

    if result.is_err() {
        log::error!("[db-client] failed to discard an undeliverable mailbox message");
    }
}

/// How many entries of one event are read per turn: small enough that a turn
/// neither holds the db loop for long nor buffers a busy event's backlog.
pub(crate) const EVENT_BATCH: usize = 5;

/// The cursor past every event currently stored, so a new subscriber sees only
/// events published after it subscribed, as on Linux. When the count fails the
/// subscription still succeeds, starting at the head of the log.
pub(crate) async fn event_tail(client: &Client, event: &Event) -> Cursor {
    let counted = client
        .send(db_models::tb_peek::Request {
            scope: scope_of_event(event.as_ref()),
            table: String::from(EVENTS_TABLE),
            cursor: None,
            limit: Some(0),
            order: None,
            count: true,
        })
        .await;

    match counted {
        Ok(Ok(response)) => Cursor::Skip(response.count.unwrap_or(0)),
        Ok(Err(err)) => {
            log::warn!(
                "[db-client] unable to count events of {}, replaying from the start: {}",
                event.as_ref(),
                err.message
            );
            Cursor::Skip(0)
        }
        Err(err) => {
            log::warn!(
                "[db-client] unable to count events of {} [zenoh issue], replaying from the start: {err}",
                event.as_ref()
            );
            Cursor::Skip(0)
        }
    }
}

/// The next batch of an event's entries after `cursor`, oldest first, read
/// without moving the cursor: the caller advances it per entry it hands over.
pub(crate) async fn read_events(
    client: &Client,
    event: &Event,
    cursor: &Cursor,
) -> Vec<(Id, Vec<u8>)> {
    let scope = scope_of_event(event.as_ref());

    let req = db_models::tx_begin::Request::routed(scope.clone());
    let tx_id = match client.send(req).await {
        Ok(Ok(response)) => response.id,
        Ok(Err(err)) => {
            log::error!("unable to start db transaction [db issue] {}", err.message);
            return Vec::new();
        }
        Err(err) => {
            log::error!("failed to begin events transaction [zenoh issue]: {err}");
            return Vec::new();
        }
    };

    let listed = client
        .send(db_models::tb_list::Request {
            id: tx_id,
            op: db_models::tb_list::Op {
                scope,
                table: String::from(EVENTS_TABLE),
                cursor: Some(cursor.clone()),
                limit: Some(EVENT_BATCH),
                order: None,
            },
        })
        .await;

    // Read-only: take a clean cut and roll it back.
    drop(
        client
            .send(db_models::tx_rollback::Request { id: tx_id })
            .await,
    );

    match listed {
        Ok(Ok(response)) => response.entities,
        Ok(Err(err)) => {
            log::error!(
                "failed to read events of {}: {}",
                event.as_ref(),
                err.message
            );
            Vec::new()
        }
        Err(err) => {
            log::error!(
                "unable to read events of {} [zenoh issue]: {err}",
                event.as_ref()
            );
            Vec::new()
        }
    }
}

/// Decodes a stored event row into the message the cell receives. An
/// undecodable row is reported and skipped; the caller moves past it, so one
/// bad row never holds back the entries behind it.
pub(crate) fn decode_event(event: &Event, id: &Id, payload: &[u8]) -> Option<CellMessage> {
    let Ok(mailbox_event) = postcard::from_bytes::<MailboxEvent>(payload) else {
        log::error!(
            "[db-client] undecodable row {id:?} in {EVENTS_TABLE} of event {}; skipping it",
            event.as_ref()
        );
        return None;
    };

    Some(CellMessage::Event {
        event: mailbox_event.event,
        payload: mailbox_event.payload,
        sender: mailbox_event.attachment.sender().map(Sri::from_uuid),
    })
}

/// Publishes a mailbox item to the DB in a transaction of its own, for items the
/// firmware emits outside any cell function.
///
/// One self-committing application: the transaction is placed, the row appended
/// and the commit taken in a single round trip.
pub(crate) async fn publish_item(client: &Client, item: MailboxItem) -> Result<(), ()> {
    let scope = item.scope();
    let op = item.into_append_op()?;

    let application = db_models::tx_apply::Request::commit_new(
        db_models::tx_begin::Constraint::Routed(scope),
        Vec::from([op.into()]),
    );

    match client.send(application).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(err)) => {
            log::error!(
                "unable to publish mailbox item [db encountered an issue] {}",
                err.message
            );
            Err(())
        }
        Err(err) => {
            log::error!(
                "failed to publish mailbox item [unable to communicate over zenoh]: {}",
                err
            );
            Err(())
        }
    }
}
