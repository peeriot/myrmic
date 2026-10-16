//! Looking up the name a cell was registered or spawned under.
//!
//! An SRI is a one-way hash of its SRN, so the name only survives as the
//! `local_name` on the cell's instance registry row. That row lives in a
//! reserved scope a cell can't read itself, so the host reads it on its behalf.

use cell_protocol::Sri;
use myrmic_common::types::error::{ENOENT, ENOMEM, GENERIC_ERROR};
use tracing::warn;
use wasmtime::Caller;

use crate::wasm::cell::state::CellState;
use crate::wasm::host_functions::{as_slice_mut, decode};

/// Writes the name the cell `sri` was registered or spawned under into the
/// guest buffer, as raw UTF-8, so the guest can size the buffer to its own
/// fixed capacity.
///
/// Returns the name's length in bytes, [`ENOENT`] when no name is recorded
/// (or the SRI is unknown), [`ENOMEM`] when the name doesn't fit the buffer,
/// or [`GENERIC_ERROR`] when the registry can't be read.
pub(crate) async fn cell_name_host(
    mut caller: Caller<'_, CellState>,
    sri_ptr: u32,
    sri_len: u32,
    buffer_ptr: u32,
    max_length: u32,
) -> i32 {
    let sri: Sri = match decode(&mut caller, sri_ptr, sri_len, "cell name lookup") {
        Ok(sri) => sri,
        Err(code) => return code,
    };

    let session = caller.data().session().clone();
    let name = match sorg_common::instance_registry::get_instance(&session, &sri).await {
        Ok(record) => record.and_then(|record| record.lineage.local_name),
        Err(err) => {
            warn!("unable to look up the name of {sri}: {err}");
            return GENERIC_ERROR;
        }
    };

    let Some(name) = name else {
        return ENOENT;
    };
    let bytes = name.as_bytes();
    if bytes.len() > max_length as usize {
        return ENOMEM;
    }
    let Ok(len) = i32::try_from(bytes.len()) else {
        return ENOMEM;
    };

    as_slice_mut(&mut caller, buffer_ptr as usize, bytes.len()).copy_from_slice(bytes);
    len
}
