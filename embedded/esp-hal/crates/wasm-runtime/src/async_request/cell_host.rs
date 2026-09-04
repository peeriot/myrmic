//! Cell Host Async Request

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use cell_protocol::Sri;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use myrmic_common::cells::{Command, CreateTimerRequest};
use wasm_runtime_macros::requests;

use crate::async_request::timers;
use crate::async_request::{
    Context, DbClientRequest, DbClientResponse, Error, Request, Response, ResponseResult,
};

use crate::CellMessage;

/// OS channel to efficiently pass unparsed Cell messages from the db-client to the runtime
pub static CELL_MSG_CHANNEL: Channel<CriticalSectionRawMutex, CellMessage, 8> = Channel::new();

/// Released once the runtime is done with a mailbox command — its removal
/// committed, or its transaction rolled back — so the poller knows the message is
/// no longer in flight and may read the mailbox again.
///
/// One signal is enough: the poller hands over a single mailbox command at a time
/// and waits here before the next, so there is never more than one in flight.
static COMMAND_HANDLED: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Releases the poller waiting on the mailbox command in flight. Fires on drop,
/// so a trapping handler, an early return, or a torn-down cell all release it.
#[derive(Debug)]
pub struct CommandHandledGuard;

impl Drop for CommandHandledGuard {
    fn drop(&mut self) {
        COMMAND_HANDLED.signal(());
    }
}

/// Waits until the runtime is done with the mailbox command in flight.
pub async fn command_handled() {
    COMMAND_HANDLED.wait().await;
}

/// Drops a completion left over from an abandoned batch or a previous cell, so
/// the next wait answers to its own command rather than a stale one.
pub fn reset_command_handled() {
    COMMAND_HANDLED.reset();
}

/// Enqueues a BLE callback for the cell to process: the runtime loop will invoke
/// the `command_<export_name>` handler with `payload` as its argument.
///
/// Non-blocking; returns `false` and drops the callback if the cell message
/// channel is full (matching the timer tick path). For inherently lossy
/// deliveries (adverts, notifications) — the caller must log a drop so it is
/// never silent (swarm#1306). Callbacks a cell *waits* on must use
/// [`enqueue_ble_callback_or_wait`] instead.
#[cfg(feature = "ble")]
pub(crate) fn enqueue_ble_callback(export_name: String, payload: Vec<u8>) -> bool {
    let msg = CellMessage::BleCallback {
        export_name,
        payload,
    };

    CELL_MSG_CHANNEL.try_send(msg).is_ok()
}

/// Enqueues a BLE callback that must not be lost, waiting for channel space if
/// necessary (swarm#1306).
///
/// A dropped request outcome or disconnect reason leaves the cell waiting
/// forever (no timeout, no error) or believing a dead link is alive, so those
/// deliveries block the BLE task until the cell message channel drains instead
/// of shedding. The wait is traced: a full channel here means the cell is not
/// keeping up.
#[cfg(feature = "ble")]
pub(crate) async fn enqueue_ble_callback_or_wait(
    site: &'static str,
    export_name: String,
    payload: Vec<u8>,
) {
    use embassy_sync::channel::TrySendError;

    let msg = CellMessage::BleCallback {
        export_name,
        payload,
    };
    if let Err(TrySendError::Full(msg)) = CELL_MSG_CHANNEL.try_send(msg) {
        let export = match &msg {
            CellMessage::BleCallback { export_name, .. } => export_name.as_str(),
            _ => "?",
        };
        log::warn!("[ble] {site} for '{export}': cell channel full — waiting to deliver");
        CELL_MSG_CHANNEL.send(msg).await;
    }
}

/// Context of a cell
#[derive(Debug)]
pub(crate) struct CellContext {
    /// Cell SRI
    pub sri: Sri,
    /// Arguments to the Cell message
    pub arguments: Option<Vec<u8>>,
    /// Command Error message
    pub error_msg: Option<String>,
    /// Commands that are available to the Cell
    pub available_commands: Vec<Command>,
}

impl Default for CellContext {
    fn default() -> Self {
        Self {
            sri: Sri::NIL,
            arguments: None,
            error_msg: None,
            available_commands: vec![],
        }
    }
}

requests! {
    wrap(CellRequest => Request::CellHost),
    unwrap(Response::CellHost => CellResponse);

    DeployCell => ResponseResult,
    GetSri => Sri,
    SetSri { sri: Sri } => (),
    SetAvailableCommands { commands: Vec<Command> } => (),
    CommandExists(Command) => bool,
    StoreErrorMessage(String) => (),
    GetErrorMessage => Option<String>,
    StoreArguments(Vec<u8>) => (),
    GetArguments => Option<Vec<u8>>,
    CreateTimer(CreateTimerRequest) => Result<u32, Error>,
    CancelTimer(u32) => ResponseResult,
}

/// Executes the cell host async request
pub(crate) async fn execute_request(ctx: &mut Context, req: CellRequest) -> CellResponse {
    log::trace!("[async req][CellHost] Received Request {req:?}");

    match req {
        CellRequest::DeployCell => {
            // Set up a new context with the given SRI registration and available commands
            ctx.cell = CellContext {
                sri: ctx.cell.sri,
                available_commands: ctx.cell.available_commands.clone(),
                ..Default::default()
            };

            // Confirm deployment to orchestration via DB
            ctx.db
                .requests
                .send(DbClientRequest::ConfirmDeployment {
                    sri: ctx.cell.sri,
                    available_commands: ctx.cell.available_commands.clone(),
                    failure: None,
                })
                .await;
            let DbClientResponse::ConfirmDeployment = ctx.db.responses.receive().await else {
                log::error!("BUG: Received wrong response to request");
                return CellResponse::DeployCell(Err(Error::Generic));
            };

            CellResponse::DeployCell(Ok(()))
        }
        CellRequest::GetSri => CellResponse::GetSri(ctx.cell.sri),
        CellRequest::SetSri { sri } => {
            ctx.cell.sri = sri;
            CellResponse::SetSri
        }
        CellRequest::SetAvailableCommands { commands } => {
            ctx.cell.available_commands = commands;
            CellResponse::SetAvailableCommands
        }
        CellRequest::CommandExists(command) => {
            CellResponse::CommandExists(ctx.cell.available_commands.iter().any(|c| c == &command))
        }
        CellRequest::StoreErrorMessage(msg) => {
            ctx.cell.error_msg = Some(msg);
            CellResponse::StoreErrorMessage
        }
        CellRequest::GetErrorMessage => CellResponse::GetErrorMessage(ctx.cell.error_msg.take()),
        CellRequest::StoreArguments(args) => {
            ctx.cell.arguments = Some(args);
            CellResponse::StoreArguments
        }
        CellRequest::GetArguments => CellResponse::GetArguments(ctx.cell.arguments.take()),
        CellRequest::CreateTimer(req) => match timers::create(ctx, req).await {
            Ok(id) => CellResponse::CreateTimer(Ok(id.0)),
            Err(err) => {
                log::warn!("[async req][CellHost] CreateTimer failed: {err}");
                ctx.cell.error_msg = Some(err);
                CellResponse::CreateTimer(Err(Error::Generic))
            }
        },
        CellRequest::CancelTimer(id) => match timers::cancel(timers::TimerId(id)).await {
            Ok(()) => CellResponse::CancelTimer(Ok(())),
            Err(err) => {
                log::warn!("[async req][CellHost] CancelTimer failed: {err}");
                ctx.cell.error_msg = Some(err);
                CellResponse::CancelTimer(Err(Error::Generic))
            }
        },
    }
}
