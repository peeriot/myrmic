use std::time::Duration;

use myrmic_common::types::error::{GENERIC_ERROR, SUCCESS};
use postcard::experimental::max_size::MaxSize;
use tracing::{debug, error};
use wasmtime::Caller;

use crate::wasm::{
    cell::state::CellState,
    host_functions::{as_slice_mut, decode, tri},
};

pub(crate) async fn wait_host(
    mut caller: Caller<'_, CellState>,
    buffer_ptr: u32,
    max_length: u32,
) -> i32 {
    let duration: Duration = tri!(decode(&mut caller, buffer_ptr, max_length, "duration"));

    debug!("host waiting for {duration:?}");
    tokio::time::sleep(duration).await;

    SUCCESS
}

pub(crate) fn now_host(caller: Caller<'_, CellState>, buffer_ptr: u32, max_length: u32) -> i32 {
    let now = caller
        .data()
        .session()
        .new_timestamp()
        .get_time()
        .to_duration();
    write_duration(caller, now, buffer_ptr, max_length)
}

pub(crate) fn uptime_host(caller: Caller<'_, CellState>, buffer_ptr: u32, max_length: u32) -> i32 {
    write_duration(
        caller,
        crate::PROCESS_START.elapsed(),
        buffer_ptr,
        max_length,
    )
}

fn write_duration(
    mut caller: Caller<'_, CellState>,
    duration: Duration,
    buffer_ptr: u32,
    max_length: u32,
) -> i32 {
    let mut buf = [0u8; Duration::POSTCARD_MAX_SIZE];
    let Ok(encoded) = postcard::to_slice(&duration, &mut buf) else {
        error!("failed to serialise duration");
        return GENERIC_ERROR;
    };
    let n_bytes = encoded.len();

    if n_bytes > max_length as usize {
        error!("duration requires more space than provided");
        return GENERIC_ERROR;
    }

    let data = as_slice_mut(&mut caller, buffer_ptr as usize, n_bytes);
    data.copy_from_slice(encoded);

    n_bytes
        .try_into()
        .expect("n_bytes is bounded by Duration::POSTCARD_MAX_SIZE")
}
