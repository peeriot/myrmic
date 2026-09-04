use crate::tri;
use crate::wasm::cell::state::CellState;
use crate::wasm::host_functions::decode;
use myrmic_common::cells::CommandRequest;
use myrmic_common::types::error::{GENERIC_ERROR, SUCCESS};
use sorg_common::{OutgoingMessage, ensure_placement_exists};
use tracing::error;
use wasmtime::Caller;

pub(crate) async fn send_command(
    mut caller: Caller<'_, CellState>,
    buffer_ptr: u32,
    length: u32,
) -> i32 {
    let req = tri!(decode(
        &mut caller,
        buffer_ptr,
        length,
        "send command request"
    ));

    match send_command_impl(&mut caller, req).await {
        Ok(()) => SUCCESS,
        Err(err) => {
            error!("{err}");
            GENERIC_ERROR
        }
    }
}

pub(super) async fn send_command_impl(
    caller: &mut Caller<'_, CellState>,
    request: CommandRequest,
) -> Result<(), &'static str> {
    // The `__sys` command space is host-emitted only; a cell must not be
    // able to spoof system notifications like cell_lost.
    if request
        .command
        .as_ref()
        .starts_with(myrmic_common::cells::SYS_COMMAND_PREFIX)
    {
        return Err("reserved system command name");
    }
    let sri = request.sri;
    let self_sri = *caller.data().sri();
    let session = caller.data().session().clone();

    // A cell can always address itself; its placement row is written by the
    // orchestrator (possibly on another node) and need not be visible in this
    // db view, so gating a self-send on it only ever false-negatives. Skipping
    // it also keeps a self-send free of reads.
    //
    // Read in a transaction of its own, not the handler's: the handler's is
    // placed on a holder of *this cell's* scope, and reading another scope
    // through it means reading whatever that node's replica of `sorg` happens
    // to hold. On the rack that was 7,190 spurious "cell not found" failures
    // in one pass — ~2.4 per success, each one a full command redelivery —
    // because spreading transactions across the mesh spreads them onto
    // replicas that have not caught up. A routed read picks the highest-head
    // holder instead, which is the freshest view available.
    // Verified once per target per cell instance: the routed read behind this
    // check was most of a sending cell's per-command wall clock at load — see
    // `CellState::verified_targets` for the measurements and the semantics.
    if sri != self_sri && !caller.data().target_verified(&sri) {
        ensure_placement_exists(&session, &sri).await?;
        caller.data_mut().mark_target_verified(sri);
    }

    tracing::debug!("sending command to {}", sri);

    let mut msg = OutgoingMessage::command(&sri, &request.command, request.payload)
        .map_err(|_| "failed to build outgoing message")?;
    caller.data().decorate_outgoing(&mut msg);

    msg.defer_into(caller.data_mut().application())
        .map_err(|e| {
            error!("cell-to-cell ff command to {sri} failed: {e}");
            "failed to send command"
        })?;

    Ok(())
}
