use alloc::borrow::ToOwned;
use alloc::string::ToString;
use core::ffi::c_int;

use cell_protocol::{CellAttachment, MailboxCommand, PLACEMENT_TABLE, placement_scope};
use db_client::v1::models::{Id, tb_get};
use myrmic_common::cells::CommandRequest;
use myrmic_common::types::error::{GENERIC_ERROR, SUCCESS};
use wasm_runtime_macros::host_function;

use crate::async_request::cell_host::CellHost;
use crate::async_request::db::DbClient;
use crate::async_request::send_request_and_wait;
use crate::imports::db::apply;

#[host_function]
fn send_command(buffer: *const u8, length: c_int) -> c_int {
    if buffer.is_null() {
        log::error!("buffer pointer is null");
        return GENERIC_ERROR;
    }

    // Deserialize command to send
    #[expect(
        clippy::cast_sign_loss,
        reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
    )]
    // safety: we already checked buffer is non-null
    let data = unsafe { core::slice::from_raw_parts(buffer, length as usize) };
    let Ok((command_request, _rest)) = postcard::take_from_bytes::<CommandRequest>(data) else {
        log::error!("failed to deserialize cell command request");
        return GENERIC_ERROR;
    };

    send_command_impl(command_request)
}

/// Sends a fire and forget command request
pub(crate) fn send_command_impl(command_request: CommandRequest) -> c_int {
    let dest_sri = command_request.sri;
    let own_sri = send_request_and_wait(CellHost::GetSri);

    // A cell can always address itself; its placement row is written by the
    // orchestrator on another node and need not be visible in this ESP's db
    // view, so gating a self-send on it only ever false-negatives.
    if dest_sri != own_sri {
        match placement_exists(dest_sri.to_string().into_bytes()) {
            Ok(false) => {
                log::error!("Destination cell {} has no placement", command_request.sri);
                return GENERIC_ERROR;
            }
            Err(e) => return e,
            _ => (),
        }
    }

    // Issue a Fire and Forget
    if send_request_and_wait(DbClient::SendCommand {
        dest_sri,
        command: MailboxCommand {
            cmd: command_request.command,
            payload: command_request.payload,
            attachment: {
                let mut attachment = CellAttachment::default();
                attachment.set_sender(Some(own_sri.as_uuid()));

                attachment
            },
        },
    })
    .is_ok()
    {
        SUCCESS
    } else {
        log::error!("failed to send fire and forget command");
        GENERIC_ERROR
    }
}

/// Checks whether the cell with the given EID has a placement
fn placement_exists(eid: Id) -> Result<bool, c_int> {
    let tb_get::Response { value } = apply(tb_get::Op {
        scope: placement_scope(),
        table: PLACEMENT_TABLE.to_owned(),
        eid,
    })?;

    Ok(value.is_some())
}
