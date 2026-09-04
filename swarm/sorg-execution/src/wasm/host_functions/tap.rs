//! Tap host functions — six Wasmtime host functions under module `"tap"`, using
//! the WAMR-compatible ABI convention (so cells built against the wasm-sdk bind
//! identically on either engine).
//!
//! All parameters are `i32` on the WASM side; pointer parameters are guest
//! linear-memory offsets interpreted by the host.  A single `Arc<TapClient>` is
//! captured per-linker at build time (D9) — there is no per-cell state.
//!
//! Return-code semantics (matching WAMR / wasm-sdk):
//!   ≥ 1     success (bytes written, or handle)
//!    0      success but empty / no data
//!   ESTALE  the handle cannot serve the request — stale/never-issued handle or
//!           IPC down; the cell should re-resolve (`ApiError::Unavailable`)
//!   -1      not found (`resolve`) or out of range (`list_entry`, where the client
//!           cannot distinguish out-of-range from IPC-down)
//!   EINVAL  a non-null scalar out-pointer (`ts_out` / `out_kind`) has a declared length
//!           smaller than the value it must hold

use std::future::Future;
use std::sync::Arc;

use myrmic_common::types::error::{EINVAL, ESTALE};
use wasmtime::{Caller, Linker};

use super::sl_claim::CellIdentity;

use crate::Result;

/// Register the six tap host functions on `linker` using the shared `tap_client`.
///
/// The module namespace is `"tap"` to match WAMR's
/// `wasm_runtime_register_natives(c"tap", ...)`.
///
/// Generic over `S` so the function can be used with any Wasmtime store state
/// (including the test stub `()`); the host functions only access guest linear
/// memory and the captured `Arc<TapClient>` — they never touch `S`.
#[allow(clippy::too_many_lines)]
#[allow(clippy::cast_sign_loss)] // WASM i32 params reinterpreted as pointer/size
#[allow(clippy::cast_possible_wrap)] // u32/u64 → i32 for WASM return values
#[allow(clippy::cast_possible_truncation)] // usize → i32 for byte counts
#[allow(clippy::needless_pass_by_value)] // Arc cloned into closures; by-value is intentional
pub fn link_tap_functions<S: CellIdentity + Send + 'static>(
    linker: &mut Linker<S>,
    tap_client: Arc<signal_layer_ipc::TapClient>,
) -> Result<()> {
    // ── tap_resolve(name_ptr: i32, name_len: i32) -> i32 ─────────────────────
    // Returns a virtual handle ≥ 1 on success, or -1 if the name is too long /
    // not found / IPC down.
    {
        let client = Arc::clone(&tap_client);
        linker.func_wrap_async(
            "tap",
            "tap_resolve",
            move |mut caller: Caller<'_, S>, (name_ptr, name_len): (i32, i32)| {
                let client = Arc::clone(&client);
                let __denied = super::sl_claim::gate(caller.data()).err();
                Box::new(async move {
                    if let Some(code) = __denied {
                        return code;
                    }
                    // Reject a negative or over-long name_len before it is used
                    // to allocate host memory or to build a request frame; the
                    // tap connection is shared by every cell, so an oversized
                    // frame would cost all of them their handles.
                    if name_len < 0 || name_len as usize > signal_layer_ipc::MAX_RESOLVE_NAME_LEN {
                        return -1i32;
                    }
                    let Some(name) =
                        read_guest_str(&mut caller, name_ptr as usize, name_len as usize)
                    else {
                        return -1i32;
                    };
                    match client.resolve(&name).await {
                        Some(vh) => vh as i32,
                        None => -1,
                    }
                }) as Box<dyn Future<Output = i32> + Send>
            },
        )?;
    }

    // ── tap_read_retained(handle, buf_ptr, buf_len, ts_out_ptr, ts_out_len) -> i32 ─
    // ts_out_ptr/ts_out_len are a WAMR `*~` pointer/length pair; when ts_out_ptr is
    // non-null the little-endian u64 timestamp is written into ts_out_ptr[0..8], which
    // requires ts_out_len == 8 (any other length returns EINVAL without writing).
    // Returns bytes written > 0, 0 if empty, ESTALE if the handle is
    // stale/never-issued or IPC is down, -1 for an OOB buffer argument.
    {
        let client = Arc::clone(&tap_client);
        linker.func_wrap_async(
            "tap",
            "tap_read_retained",
            move |mut caller: Caller<'_, S>,
                  (handle, buf_ptr, buf_len, ts_out_ptr, ts_out_len): (i32, i32, i32, i32, i32)| {
                let client = Arc::clone(&client);
                let __denied = super::sl_claim::gate(caller.data()).err();
                Box::new(async move {
                    use signal_layer_ipc::ClientRead;
                    if let Some(code) = __denied {
                        return code;
                    }
                    // Reject negative buf_len before any cast to usize.
                    if buf_len < 0 {
                        return -1i32;
                    }
                    let vh = handle as u32;
                    match client.read_retained(vh).await {
                        ClientRead::Value {
                            timestamp_ms,
                            bytes,
                        } => {
                            let n = bytes.len().min(buf_len as usize);
                            if n > 0
                                && write_guest_bytes(&mut caller, buf_ptr as usize, &bytes[..n])
                                    .is_err()
                            {
                                return -1;
                            }
                            if ts_out_ptr != 0 {
                                // The guest-declared range must hold the whole 8-byte
                                // timestamp
                                if ts_out_len < 0
                                    || (ts_out_len as usize) != std::mem::size_of::<i64>()
                                {
                                    return EINVAL;
                                }
                                #[allow(clippy::cast_possible_wrap)]
                                let ts_i64 = timestamp_ms as i64;
                                if write_guest_i64(&mut caller, ts_out_ptr as usize, ts_i64)
                                    .is_err()
                                {
                                    return -1;
                                }
                            }
                            n as i32
                        }
                        ClientRead::Empty => 0,
                        ClientRead::Unavailable => ESTALE,
                    }
                }) as Box<dyn Future<Output = i32> + Send>
            },
        )?;
    }

    // ── tap_take_event(handle: i32, buf_ptr: i32, buf_len: i32) -> i32 ───────
    // Returns bytes written > 0, 0 if empty, ESTALE if the handle is
    // stale/never-issued or IPC is down, -1 for an OOB buffer argument.
    {
        let client = Arc::clone(&tap_client);
        linker.func_wrap_async(
            "tap",
            "tap_take_event",
            move |mut caller: Caller<'_, S>, (handle, buf_ptr, buf_len): (i32, i32, i32)| {
                let client = Arc::clone(&client);
                let __denied = super::sl_claim::gate(caller.data()).err();
                Box::new(async move {
                    use signal_layer_ipc::ClientRead;
                    if let Some(code) = __denied {
                        return code;
                    }
                    // Reject negative buf_len before any cast to usize.
                    if buf_len < 0 {
                        return -1i32;
                    }
                    let vh = handle as u32;
                    match client.take_event(vh).await {
                        ClientRead::Value { bytes, .. } => {
                            let n = bytes.len().min(buf_len as usize);
                            if n > 0
                                && write_guest_bytes(&mut caller, buf_ptr as usize, &bytes[..n])
                                    .is_err()
                            {
                                return -1;
                            }
                            n as i32
                        }
                        ClientRead::Empty => 0,
                        ClientRead::Unavailable => ESTALE,
                    }
                }) as Box<dyn Future<Output = i32> + Send>
            },
        )?;
    }

    // ── tap_drain_batch(handle: i32, buf_ptr: i32, buf_len: i32) -> i32 ──────
    // Batch taps are a stub in v1 (D1): 0 for any well-formed call, -1 for a
    // negative buf_len so length rejection is uniform across the tap ABI.
    {
        linker.func_wrap_async(
            "tap",
            "tap_drain_batch",
            move |caller: Caller<'_, S>, (_handle, _buf_ptr, buf_len): (i32, i32, i32)| {
                let __denied = super::sl_claim::gate(caller.data()).err();
                Box::new(async move {
                    if let Some(code) = __denied {
                        return code;
                    }
                    if buf_len < 0 { -1i32 } else { 0i32 }
                }) as Box<dyn Future<Output = i32> + Send>
            },
        )?;
    }

    // ── tap_list_len() -> i32 ─────────────────────────────────────────────────
    // Returns the tap count, or ESTALE if IPC is down.
    {
        let client = Arc::clone(&tap_client);
        linker.func_wrap_async(
            "tap",
            "tap_list_len",
            move |caller: Caller<'_, S>, (): ()| {
                let client = Arc::clone(&client);
                let __denied = super::sl_claim::gate(caller.data()).err();
                Box::new(async move {
                    if let Some(code) = __denied {
                        return code;
                    }
                    match client.list_len().await {
                        Some(count) => count as i32,
                        // `list_len`'s None has no out-of-range case: it always
                        // means the call timed out or the server is unreachable.
                        None => ESTALE,
                    }
                }) as Box<dyn Future<Output = i32> + Send>
            },
        )?;
    }

    // ── tap_list_entry(index, name_ptr, name_len, out_kind_ptr, out_kind_len) -> i32 ─
    // out_kind_ptr/out_kind_len are a WAMR `*~` pointer/length pair; the wire kind is
    // u8, widened to a little-endian i32 written into out_kind_ptr[0..4], which requires
    // out_kind_len == 4 (any other length returns EINVAL without writing).
    // Returns bytes written, -1 if out of range, OOB, or IPC down.
    {
        let client = Arc::clone(&tap_client);
        linker.func_wrap_async(
            "tap",
            "tap_list_entry",
            move |mut caller: Caller<'_, S>,
                  (index, name_ptr, name_len, out_kind_ptr, out_kind_len): (
                i32,
                i32,
                i32,
                i32,
                i32,
            )| {
                let client = Arc::clone(&client);
                let __denied = super::sl_claim::gate(caller.data()).err();
                Box::new(async move {
                    if let Some(code) = __denied {
                        return code;
                    }
                    // Reject negative name_len before any cast to usize.
                    if name_len < 0 {
                        return -1i32;
                    }
                    match client.list_entry(index as u32).await {
                        Some((name, kind)) => {
                            let bytes = name.as_bytes();
                            let n = bytes.len().min(name_len as usize);
                            if name_ptr != 0
                                && n > 0
                                && write_guest_bytes(&mut caller, name_ptr as usize, &bytes[..n])
                                    .is_err()
                            {
                                return -1;
                            }
                            if out_kind_ptr != 0 {
                                // The guest-declared range must hold the whole 4-byte
                                // kind; reject a short (or negative) length rather than
                                // writing past what the guest reserved.
                                if out_kind_len < 0
                                    || (out_kind_len as usize) != std::mem::size_of::<i32>()
                                {
                                    return EINVAL;
                                }
                                // Widen u8 → i32 deliberately (SR-10 / plan spec).
                                if write_guest_i32(
                                    &mut caller,
                                    out_kind_ptr as usize,
                                    i32::from(kind),
                                )
                                .is_err()
                                {
                                    return -1;
                                }
                            }
                            n as i32
                        }
                        // The client cannot distinguish out-of-range from
                        // IPC-down here, and -1 is the SDK's end-of-list
                        // sentinel — so this stays -1 rather than ESTALE.
                        None => -1,
                    }
                }) as Box<dyn Future<Output = i32> + Send>
            },
        )?;
    }

    // ── tap_type_id(handle: i32, out_id_ptr: i32) -> i32 ─────────────────────
    // out_id_ptr is a guest *mut u32 (little-endian). Returns 0 on success,
    // -1 for a stale/unknown handle, OOB pointer, or IPC down. Same ABI as the
    // WAMR host (swarm#1315); the SDK calls it once at resolve time.
    {
        let client = Arc::clone(&tap_client);
        linker.func_wrap_async(
            "tap",
            "tap_type_id",
            move |mut caller: Caller<'_, S>, (handle, out_id_ptr, out_id_len): (i32, i32, i32)| {
                let client = Arc::clone(&client);
                Box::new(async move {
                    if out_id_ptr == 0 {
                        return -1i32;
                    }
                    // The guest-declared range must hold the whole 4-byte id;
                    // reject a short (or negative) length rather than writing past it.
                    if out_id_len < 0 || (out_id_len as usize) != std::mem::size_of::<u32>() {
                        return EINVAL;
                    }
                    match client.tap_type_id(handle as u32).await {
                        Some(id) => {
                            if write_guest_bytes(
                                &mut caller,
                                out_id_ptr as usize,
                                &id.to_le_bytes(),
                            )
                            .is_err()
                            {
                                return -1;
                            }
                            0
                        }
                        None => -1,
                    }
                }) as Box<dyn Future<Output = i32> + Send>
            },
        )?;
    }

    Ok(())
}

// ── Guest-memory helpers (shared with the outlet host functions) ─────────────

/// Read a UTF-8 string from guest linear memory.  Returns `None` if the range
/// is out of bounds or the bytes are not valid UTF-8.
pub(super) fn read_guest_str<S>(
    caller: &mut Caller<'_, S>,
    ptr: usize,
    len: usize,
) -> Option<String> {
    let memory = caller.get_export("memory")?.into_memory()?;
    let data = memory.data(caller);
    let end = ptr.checked_add(len)?;
    if end > data.len() {
        return None;
    }
    std::str::from_utf8(&data[ptr..end]).ok().map(str::to_owned)
}

/// Read raw bytes from guest linear memory.  Returns `None` if the range is
/// out of bounds.
pub(super) fn read_guest_bytes<S>(
    caller: &mut Caller<'_, S>,
    ptr: usize,
    len: usize,
) -> Option<Vec<u8>> {
    let memory = caller.get_export("memory")?.into_memory()?;
    let data = memory.data(caller);
    let end = ptr.checked_add(len)?;
    if end > data.len() {
        return None;
    }
    Some(data[ptr..end].to_vec())
}

/// Write bytes into guest linear memory at `ptr`.
///
/// Returns `Ok(())` on success or `Err(())` if the range `ptr..ptr+bytes.len()`
/// is out of bounds — the caller must propagate this as -1 to the guest.
/// Fail-closed: no partial write is performed on bounds failure.
pub(super) fn write_guest_bytes<S>(
    caller: &mut Caller<'_, S>,
    ptr: usize,
    bytes: &[u8],
) -> Result<(), ()> {
    if bytes.is_empty() {
        return Ok(());
    }
    let memory = caller
        .get_export("memory")
        .and_then(wasmtime::Extern::into_memory)
        .ok_or(())?;
    let end = ptr.checked_add(bytes.len()).ok_or(())?;
    let data = memory.data_mut(caller);
    if end > data.len() {
        return Err(());
    }
    data[ptr..end].copy_from_slice(bytes);
    Ok(())
}

/// Write an `i64` (little-endian) to guest memory at `ptr`.
/// Returns `Err(())` if the 8-byte write would be out of bounds.
fn write_guest_i64<S>(caller: &mut Caller<'_, S>, ptr: usize, value: i64) -> Result<(), ()> {
    write_guest_bytes(caller, ptr, &value.to_le_bytes())
}

/// Write an `i32` (little-endian) to guest memory at `ptr`.
/// Returns `Err(())` if the 4-byte write would be out of bounds.
pub(super) fn write_guest_i32<S>(
    caller: &mut Caller<'_, S>,
    ptr: usize,
    value: i32,
) -> Result<(), ()> {
    write_guest_bytes(caller, ptr, &value.to_le_bytes())
}
