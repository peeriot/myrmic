use myrmic_common::cells::CreateTimerRequest;
use myrmic_common::types::error::{GENERIC_ERROR, SUCCESS};
use tracing::error;
use wasmtime::Caller;

use crate::tri;
use crate::wasm::{cell::state::CellState, host_functions::decode};

pub(crate) async fn create_timer(
    mut caller: Caller<'_, CellState>,
    buffer_ptr: u32,
    length: u32,
) -> i32 {
    let request: CreateTimerRequest = tri!(decode(
        &mut caller,
        buffer_ptr,
        length,
        "create timer request"
    ));

    // Validate that the target handler export exists. The timer carries the bare
    // command name; the export symbol is `command_<name>` (the same mapping the
    // command dispatch path applies), and it takes the 5-arg handler ABI.
    let handler_export = format!("command_{}", request.export_name);
    if caller.get_export(&handler_export).is_none() {
        error!("timer export '{handler_export}' does not exist on module");
        return GENERIC_ERROR;
    }

    match caller.data_mut().create_timer(request) {
        Ok(id) => id.cast_signed(),
        Err(err) => {
            error!("failed to create timer: {err}");
            GENERIC_ERROR
        }
    }
}

pub(crate) async fn cancel_timer(mut caller: Caller<'_, CellState>, id: u32) -> i32 {
    match caller.data_mut().cancel_timer(id) {
        Ok(()) => SUCCESS,
        Err(err) => {
            error!("failed to cancel timer: {err}");
            GENERIC_ERROR
        }
    }
}
