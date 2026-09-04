use myrmic_common::types::error::GENERIC_ERROR;
use wasmtime::Caller;

use crate::wasm::cell::state::CellState;
use crate::wasm::host_functions::as_slice_mut;
use tracing::error;

#[allow(clippy::cast_possible_truncation)]
#[allow(clippy::cast_possible_wrap)]
pub(super) fn get_arguments(
    mut caller: Caller<'_, CellState>,
    buffer_ptr: u32,
    max_length: u32,
) -> i32 {
    let payload = caller.data_mut().take_arguments();

    let Some(bytes) = payload else {
        return 0;
    };

    let n_bytes = bytes.len();

    if n_bytes > (max_length as usize) {
        error!(
            "input requires more space than provided by the cell module {id}; provided: {max_length}; required: {n_bytes}",
            id = caller.data().sri()
        );
        return GENERIC_ERROR;
    }

    let data = as_slice_mut(&mut caller, buffer_ptr as usize, n_bytes);

    // Safety: we're grabbing a slice based on the payload length
    data.copy_from_slice(&bytes);

    n_bytes as i32
}
