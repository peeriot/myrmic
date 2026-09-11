//! Host functions exposing facts about this runtime to the cells it hosts: its
//! effective tag set and its runtime id.
//!
//! The node's [`LiveTags`] handle is captured in the linker closures that
//! register these functions (see `link_cell_functions`), so each runtime serves
//! its own tags and a retag is reflected on the next call.

use cell_protocol::RuntimeId;
use cell_protocol::node_tags::LiveTags;
use myrmic_common::types::error::{ENOMEM, GENERIC_ERROR, SUCCESS};
use tracing::error;
use wasmtime::Caller;

use crate::wasm::{cell::state::CellState, host_functions::as_slice_mut};

pub(crate) fn runtime_tags_len(tags: &LiveTags) -> i32 {
    match encode_tags(tags) {
        Some(encoded) => encoded.len().try_into().unwrap_or(GENERIC_ERROR),
        None => GENERIC_ERROR,
    }
}

pub(crate) fn runtime_tags_host(
    mut caller: Caller<'_, CellState>,
    tags: &LiveTags,
    buffer_ptr: u32,
    max_length: u32,
) -> i32 {
    let Some(encoded) = encode_tags(tags) else {
        return GENERIC_ERROR;
    };

    write_bytes(&mut caller, &encoded, buffer_ptr, max_length)
}

pub(crate) fn runtime_id_host(
    mut caller: Caller<'_, CellState>,
    buffer_ptr: u32,
    max_length: u32,
) -> i32 {
    let id: RuntimeId = caller.data().session().zid().into();
    let id = id.to_string();
    let encoded = match postcard::to_allocvec(&id) {
        Ok(bytes) => bytes,
        Err(err) => {
            error!("failed to serialise runtime id: {err}");

            return GENERIC_ERROR;
        }
    };

    write_bytes(&mut caller, &encoded, buffer_ptr, max_length)
}

/// Serializes the node's current effective tags, or `None` (having logged) when
/// they cannot be encoded.
fn encode_tags(tags: &LiveTags) -> Option<Vec<u8>> {
    let tags = tags.get();

    match postcard::to_allocvec(&*tags) {
        Ok(bytes) => Some(bytes),
        Err(err) => {
            error!("failed to serialise runtime tags: {err}");

            None
        }
    }
}

/// Copies `encoded` into the guest buffer, returning [`SUCCESS`], or [`ENOMEM`]
/// when the buffer is too small. The guest decodes the length-delimited payload
/// without a written-length hint, so only success/failure is reported.
fn write_bytes(
    caller: &mut Caller<'_, CellState>,
    encoded: &[u8],
    buffer_ptr: u32,
    max_length: u32,
) -> i32 {
    if encoded.len() > max_length as usize {
        return ENOMEM;
    }

    let data = as_slice_mut(caller, buffer_ptr as usize, encoded.len());
    data.copy_from_slice(encoded);

    SUCCESS
}
