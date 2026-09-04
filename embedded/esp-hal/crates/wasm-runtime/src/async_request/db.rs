//! DB Async Request
//!
//! Db work is expressed as operations against the running cell function's
//! *application* — one batched transaction, held by the cell-db-service task
//! because that is where the client lives. `Open` costs nothing (no I/O until
//! the first flush) and `Defer` costs nothing but this channel hop, so a
//! handler that only writes pays a single round trip, at `Commit`. `Apply` is
//! for operations whose value the guest reads back: it flushes whatever is
//! deferred with itself last, which is also what keeps program order.
//!
//! `Defer` still answers with a result: buffering is refused once an earlier
//! operation has aborted the function's transaction, and the guest has to hear
//! that rather than a success for a write that can never commit.
//!
//! `ReadIn` is the exception to all of that — a one-shot read in a transaction
//! of its own, placed on a holder of the scope it names, for the reads that
//! must not be routed by whatever the current application happens to be.

use alloc::string::String;
use alloc::vec::Vec;

use cell_protocol::{MailboxCommand, MailboxEvent, Sri};
use db_client::application::Error as ApplyError;
use db_client::v1::models::{Scope, TxOp, TxOpResponse};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Receiver, Sender};
use myrmic_common::cells::{Command, Event};

use wasm_runtime_macros::requests;

use crate::async_request::{Context, Request, Response, ResponseResult};

/// Context of a DB client
#[derive(Debug)]
pub(crate) struct DbContext {
    /// Client requests
    pub requests: Sender<'static, CriticalSectionRawMutex, DbClientRequest, 1>,
    /// Client responses
    pub responses: Receiver<'static, CriticalSectionRawMutex, DbClientResponse, 1>,
}

requests! {
    wrap(DbClientRequest => DbRequest::DbClient),
    unwrap(DbResponse::DbClient => DbClientResponse);

    Open(Scope) => (),
    Defer(TxOp) => Result<(), ApplyError>,
    Apply(TxOp) => Result<TxOpResponse, ApplyError>,
    ReadIn { scope: Scope, op: TxOp } => Result<TxOpResponse, ApplyError>,
    Commit => Result<(), ApplyError>,
    Rollback => (),
    ConfirmDeployment { sri: Sri, available_commands: Vec<Command>, failure: Option<String> } => (),
    ConfirmDeletion => (),
    SendCommand { dest_sri: Sri, command: MailboxCommand } => ResponseResult,
    PublishEvent { event: MailboxEvent } => ResponseResult,
    SubscribeEvent(Event) => (),
    UnsubscribeEvent(Event) => (),
}

requests! {
    wrap(DbRequest => Request::DB),
    unwrap(Response::DB => DbResponse);

    category DbClient(DbClientRequest) => DbClientResponse,
}

/// Executes the DB async request
pub(crate) async fn execute_request(ctx: &mut Context, req: DbRequest) -> DbResponse {
    log::trace!("[async req][DB] Received Request {req:?}");

    match req {
        DbRequest::DbClient(client_req) => {
            // Simple DB bridge
            ctx.db.requests.send(client_req).await;

            DbResponse::DbClient(ctx.db.responses.receive().await)
        }
    }
}
