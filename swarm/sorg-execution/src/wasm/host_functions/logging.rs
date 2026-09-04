use core::str;
use std::time::Instant;

use myrmic_common::types::error::{GENERIC_ERROR, SUCCESS};
use tracing::{Level, debug, error, info, span, trace, warn};
use wasmtime::Caller;

use crate::wasm::cell::cell_task::metrics;
use crate::wasm::cell::state::CellState;

use super::as_slice;

pub(super) fn log(
    mut caller: Caller<'_, CellState>,
    buffer_ptr: u32,
    length: u32,
    log_level: u32,
) -> i32 {
    let entered = Instant::now();
    let sri = *caller.data().sri();
    let zid = caller.data().session().zid();
    let uptime = crate::PROCESS_START.elapsed();
    let uptime_secs = uptime.as_secs();
    let uptime_subsec_nanos = uptime.subsec_nanos();
    let parent = caller.data().current_span();
    let data = as_slice(&mut caller, buffer_ptr as usize, length as usize);

    let Ok(msg) = str::from_utf8(data) else {
        error!("module logged using and invalid string");
        return GENERIC_ERROR;
    };

    let guard = span!(parent: &parent, Level::INFO, "wasm_log").entered();
    match log_level {
        0 => trace!(%sri, %uptime_secs, %uptime_subsec_nanos, %zid, "{msg}"),
        1 => debug!(%sri, %uptime_secs, %uptime_subsec_nanos, %zid, "{msg}"),
        2 => info!(%sri, %uptime_secs, %uptime_subsec_nanos, %zid, "{msg}"),
        3 => warn!(%sri, %uptime_secs, %uptime_subsec_nanos, %zid, "{msg}"),
        4 => error!(%sri, %uptime_secs, %uptime_subsec_nanos, %zid, "{msg}"),
        _ => unreachable!("wrong log level"),
    }
    // Closed before the timer reads, so the appender's work lands inside the
    // measurement rather than after it.
    drop(guard);

    metrics::record_host_log(&sri, entered.elapsed());
    SUCCESS
}
