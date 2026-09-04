use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use cell_protocol::{DeploymentConfirmation, Sri};
use db_client::application::{Application, Error as ApplyError};
use db_client::v1::Client;
use db_client::v1::models::Cursor;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Sender;
use embassy_time::with_timeout;
use myrmic_common::cells::{Command, Event};
use wasm_runtime::async_request::{DbClientRequest, DbClientResponse, Error};
use zenoh_nano::scout::ZenohIdProto;

use crate::mailbox::MailboxItem;
use crate::service::DEFAULT_TIMEOUT;

/// The application of the cell function currently running, which every db
/// request from the runtime joins.
///
/// Absent means the runtime asked for db work outside a cell function, which is
/// a bug in the runtime rather than something a guest can provoke — the
/// dispatcher opens one before it calls anything.
fn current(app: &mut Option<Application>) -> Result<&mut Application, ApplyError> {
    app.as_mut().ok_or_else(|| {
        log::error!("BUG: db work outside a cell function; no application is open");
        ApplyError::Unreachable(String::from("no application is open"))
    })
}

/// Buffers the write that delivers a mailbox item into the running cell
/// function's application.
fn defer_item(app: &mut Option<Application>, item: MailboxItem) -> Result<(), ()> {
    let op = item.into_append_op()?;

    current(app)
        .and_then(|app| app.defer(op))
        .map_err(|err| log::error!("[db] unable to defer a mailbox item: {err}"))
}

/// Runs one application round trip under the standard timeout.
///
/// Boxed so the future doesn't enlarge `handle`'s poll frame — the big `match`
/// would otherwise size for the worst inline arm. A timeout leaves the
/// transaction's fate unknown, exactly like a transport error, so the
/// application is abandoned to the server's idle timeout.
///
/// Dropping the round trip mid-flight is what makes this safe to wrap: the
/// application poisons itself for the duration of a flush, so a timeout here
/// cannot leave one that has silently lost its deferred writes and would still
/// commit. Everything after it, `Commit` included, fails.
async fn timed<T>(
    what: &str,
    op: impl Future<Output = Result<T, ApplyError>>,
) -> Result<T, ApplyError> {
    match with_timeout(DEFAULT_TIMEOUT, Box::pin(op)).await {
        Ok(result) => result,
        Err(_) => {
            log::warn!("[db] {what} timed out");
            Err(ApplyError::Unreachable(String::from("timeout")))
        }
    }
}

/// Handles the DB client requests
#[expect(
    clippy::too_many_lines,
    reason = "There is no way to shorten the match statement"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "The handler threads the session's mutable loop state"
)]
pub(crate) async fn handle(
    client: &Client,
    zid: ZenohIdProto,
    db_client_req: DbClientRequest,
    cell: &mut Option<(Sri, Vec<Command>)>,
    db_responses: Sender<'static, CriticalSectionRawMutex, DbClientResponse, 1>,
    subscribed_events: &mut Vec<(Event, Cursor)>,
    awaiting_deletion_confirmation: &mut bool,
    watched: &mut Option<cell_protocol::supervision::WatchedCell>,
    application: &mut Option<Application>,
) {
    match db_client_req {
        DbClientRequest::ConfirmDeployment {
            sri,
            available_commands,
            failure,
        } => {
            *cell = Some((sri, available_commands));
            if failure.is_some() {
                // Nothing is running; there is nothing to fence.
                *watched = None;
            }

            crate::deploy::confirm_deployment(
                client,
                zid,
                DeploymentConfirmation::Deployed { failure, sri },
            )
            .await;

            db_responses.send(DbClientResponse::ConfirmDeployment).await;
        }
        DbClientRequest::ConfirmDeletion => {
            if *awaiting_deletion_confirmation {
                *watched = None;
                if let Some((sri, _)) = cell.take() {
                    crate::deploy::confirm_deployment(
                        client,
                        zid,
                        DeploymentConfirmation::Deleted { sri },
                    )
                    .await;
                } else {
                    log::error!(
                        "BUG: Received `ConfirmDeletion` request without a cell deployed. \
                    This should've been rejected by the deploy handler"
                    );
                }
                *awaiting_deletion_confirmation = false;
            }

            db_responses.send(DbClientResponse::ConfirmDeletion).await;
        }
        // Opening costs nothing: no transaction is placed until something is
        // applied. A previous application still sitting here was never closed,
        // which only a runtime bug produces — drop it and say so, rather than
        // silently folding the last function's leftovers into this one.
        DbClientRequest::Open(scope) => {
            if application.is_some() {
                log::error!("BUG: opening an application over one that was never closed");
            }

            *application = Some(Application::routed(client.clone(), scope));
            db_responses.send(DbClientResponse::Open).await;
        }
        DbClientRequest::Defer(op) => {
            let deferred = current(application).and_then(|app| app.defer_op(op));

            db_responses.send(DbClientResponse::Defer(deferred)).await;
        }
        DbClientRequest::Apply(op) => {
            let name = op.name();
            let applied = match current(application) {
                Ok(app) => timed(name, app.apply_op(op)).await,
                Err(err) => Err(err),
            };

            db_responses.send(DbClientResponse::Apply(applied)).await;
        }
        // Placed on a holder of the scope it actually names, in a transaction
        // of its own. A cell function's application is routed by the *cell's*
        // scope, and reading another scope through it reads whatever that
        // node's replica of that scope happens to hold — for `sorg` that is a
        // replica which may not have caught up. Read access, so a fallback
        // landing mints no sink, and self-closing, so it costs one round trip.
        DbClientRequest::ReadIn { scope, op } => {
            let name = op.name();
            let mut app = Application::routed(client.clone(), scope).read_only();
            let read = timed(name, app.apply_and_commit(op)).await;

            db_responses.send(DbClientResponse::ReadIn(read)).await;
        }
        DbClientRequest::Commit => {
            let committed = match application.take() {
                Some(app) => timed("commit", app.commit()).await,
                None => {
                    log::error!("BUG: committing when no application is open");
                    Err(ApplyError::Unreachable(String::from(
                        "no application is open",
                    )))
                }
            };

            db_responses.send(DbClientResponse::Commit(committed)).await;
        }
        DbClientRequest::Rollback => {
            if let Some(app) = application.take()
                && let Err(err) = timed("rollback", app.rollback()).await
            {
                log::warn!("[db] unable to roll back the application: {err}");
            }

            db_responses.send(DbClientResponse::Rollback).await;
        }
        DbClientRequest::SendCommand { dest_sri, command } => {
            // Reserved system names are host-emitted only, so a guest cannot
            // spoof supervision notifications (mirrors the Linux host).
            if command
                .cmd
                .as_ref()
                .starts_with(myrmic_common::cells::SYS_COMMAND_PREFIX)
            {
                log::warn!("[db-client] guest tried to send reserved command {command:?}");
                db_responses
                    .send(DbClientResponse::SendCommand(Err(Error::Generic)))
                    .await;
                return;
            }
            let name = command.clone();
            // Joins the cell function's application: the command is only
            // delivered if that function's work commits, and costs nothing
            // until it does.
            let item = MailboxItem::Command { dest_sri, command };
            let sent = defer_item(application, item).map_err(|()| {
                log::error!("[db-client] Failed to publish command {name:?}");
                Error::Generic
            });

            db_responses.send(DbClientResponse::SendCommand(sent)).await;
        }
        DbClientRequest::PublishEvent { event } => {
            let name = event.event.clone();
            let published = defer_item(application, MailboxItem::Event(event)).map_err(|()| {
                log::error!("[db-client] Failed to publish event {name:?}");
                Error::Generic
            });

            db_responses
                .send(DbClientResponse::PublishEvent(published))
                .await;
        }
        DbClientRequest::SubscribeEvent(ev) => {
            log::info!("Subscribed to event {}", ev.as_ref());
            subscribed_events.push((ev, Cursor::Skip(0)));

            db_responses.send(DbClientResponse::SubscribeEvent).await;
        }
        DbClientRequest::UnsubscribeEvent(ev) => {
            log::info!("Unsubscribed to event {}", ev.as_ref());
            subscribed_events.retain(|(e, _)| e != &ev);

            db_responses.send(DbClientResponse::UnsubscribeEvent).await;
        }
    }
}
