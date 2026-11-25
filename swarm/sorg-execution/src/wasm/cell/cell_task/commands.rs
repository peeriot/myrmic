use std::fmt::Debug;
use std::time::Instant;

use cell_protocol::Sri;
use myrmic_common::cells::Command;
use tracing::debug;
use wasmtime::{Instance, Store, WasmParams};

use crate::wasm::cell::cell_task::{Outcome, log_crash, metrics};
use crate::wasm::cell::state::CellState;
use crate::wasm::cell::{CellCommand, CommandOrigin, sri_parts};

/// Why a command export did not complete. A trap is not a `CellCommandError`
/// but the end of the incarnation.
enum CommandFailure {
    /// The cell exports no handler for this command.
    NotPresent,
    /// The handler returned an error code, or its arguments could not be
    /// handed to it.
    Failed(String),
    /// The call itself errored: the guest trapped.
    Trapped(wasmtime::Error),
}

/// Runs a command's handler. A failed or trapped run rolls the command's
/// transaction back, leaving it in the mailbox; a trapped one also ends the
/// incarnation.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub(super) async fn handle_command(
    instance: &Instance,
    store: &mut Store<CellState>,
    sri: &Sri,
    cmd: CellCommand,
) -> Outcome {
    let CellCommand {
        cmd,
        payload,
        origin,
        ready: _,
        sender,
    } = cmd;

    tracing::debug!("cell '{sri}' received cmd '{c}'", c = cmd.as_ref());

    let payload = payload.unwrap_or_default();

    // Commands are fire-and-forget: there is nobody to hand the error back to,
    // so log it and let the rolled-back transaction be the real consequence.
    match execute_command_export(payload, store, instance, sri, &cmd, sender).await {
        Ok(()) => Outcome::of(consume(store, origin)),
        // A command this cell does not serve can never succeed, so consume it
        // rather than leave it redelivering forever. Anything else is rolled
        // back and comes round again.
        Err(CommandFailure::NotPresent) => {
            tracing::warn!(
                "cell '{sri}' does not serve cmd '{c}'; discarding it",
                c = cmd.as_ref()
            );
            Outcome::of(consume(store, origin))
        }
        Err(CommandFailure::Failed(reason)) => {
            tracing::error!(
                "cell '{sri}' error processing cmd '{c}': {reason}",
                c = cmd.as_ref()
            );
            Outcome::Failed
        }
        Err(CommandFailure::Trapped(trap)) => {
            log_crash(sri, "command", cmd.as_ref(), &trap);
            Outcome::Trapped
        }
    }
}

/// Removes the command from the mailbox inside the handler's transaction, so it
/// is consumed only if the handler's work commits. Buffered, so it costs no
/// round trip of its own — it applies with the handler's commit.
fn consume(store: &mut Store<CellState>, origin: CommandOrigin) -> bool {
    let CommandOrigin::Mailbox(receipt) = origin else {
        return true;
    };

    // Refused only when the handler's transaction is already gone, in which
    // case nothing it did is going to commit either — fail the turn so the
    // command is retried rather than dropped.
    if let Err(err) = receipt.consume_in_tx(store.data_mut().application()) {
        tracing::error!("unable to consume the command in the handler transaction: {err}");
        return false;
    }

    true
}

async fn execute_command_export(
    payload: Vec<u8>,
    store: &mut Store<CellState>,
    instance: &Instance,
    sri: &Sri,
    command: &Command,
    sender: Option<uuid::Uuid>,
) -> Result<(), CommandFailure> {
    let export_name = cmd_fn_name(command);
    if export_name == ON_CELL_LOST_EXPORT
        && instance
            .get_func(&mut *store, ON_CELL_LOST_EXPORT)
            .is_none()
    {
        // No handler = the loss is simply dropped (temporary semantics).
        tracing::debug!("cell '{sri}' has no on_cell_lost handler; dropping notification");
        return Ok(());
    }

    // The cell's own identity and its caller's identity, split into the
    // `(hi, lo)` i64 pairs the guest recombines into `Metadata`.
    let (id_hi, id_lo) = sri.to_parts();
    let (sender_hi, sender_lo) = sri_parts(sender);

    let n_bytes: i32 = payload
        .len()
        .try_into()
        .expect("arguments should be reasonably small");
    store
        .data_mut()
        .store_arguments(payload)
        .map_err(|err| CommandFailure::Failed(format!("failed to store arguments: {err}")))?;

    let exit_code = run_command_export(
        &export_name,
        store,
        instance,
        sri,
        command,
        (id_hi, id_lo, sender_hi, sender_lo, n_bytes),
    )
    .await?;

    if exit_code != 0 {
        // handler code signals error -> report error without running deferred actions
        let stored_err_msg = store
            .data_mut()
            .take_err_msg()
            .unwrap_or("Module error; No err msg stored".to_owned());
        return Err(CommandFailure::Failed(stored_err_msg));
    }

    Ok(())
}

async fn run_command_export<P>(
    export_name: &str,
    store: &mut Store<CellState>,
    instance: &Instance,
    sri: &Sri,
    command: &Command,
    params: P,
) -> Result<i32, CommandFailure>
where
    P: WasmParams + Send + Sync + Debug,
{
    debug!(
        "executing export of cell {sri} - export name: '{export_name}'; parameters: '{params:?}'"
    );
    let looked_up = Instant::now();
    let cmd_handler = match instance.get_typed_func::<P, i32>(&mut (*store), export_name) {
        Ok(function) => function,
        Err(err) => {
            tracing::warn!(
                "cell '{sri}' does not offer the requested command '{c}' which could accept the parameters '{params:?}' -- {err}",
                c = command.as_ref()
            );
            return Err(CommandFailure::NotPresent);
        }
    };
    let lookup = looked_up.elapsed();

    let called = Instant::now();
    let exit_code = cmd_handler.call_async(&mut (*store), params).await;
    metrics::record_dispatch_split(sri, lookup, called.elapsed());

    exit_code.map_err(CommandFailure::Trapped)
}

pub(crate) fn cmd_fn_name(cmd: &Command) -> String {
    // System notifications route to their reserved export, never to a
    // user `command_*` handler.
    if cmd.as_ref() == myrmic_common::cells::SYS_CELL_LOST {
        return ON_CELL_LOST_EXPORT.to_owned();
    }
    format!("command_{cmd_name}", cmd_name = cmd.as_ref())
}

pub(crate) const ON_CELL_LOST_EXPORT: &str = "on_cell_lost";
