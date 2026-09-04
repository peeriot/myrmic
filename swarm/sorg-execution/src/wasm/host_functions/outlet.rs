//! Outlet host functions — four Wasmtime host functions under module
//! `"outlet"`, using the WAMR-compatible ABI convention (so cells built
//! against myrmic-sdk bind identically on either engine; the WAMR counterpart
//! is `wasm-runtime/src/imports/outlet.rs`).
//!
//! All parameters are `i32` on the WASM side; pointer parameters are guest
//! linear-memory offsets interpreted by the host.  The same `Arc<TapClient>`
//! the tap functions capture serves outlets too — one shared IPC connection,
//! with tap and outlet handles kept in separate families by the client.
//!
//! Return-code semantics (matching WAMR / myrmic-sdk):
//!   `outlet_resolve`        → handle ≥ 0, or -1 (not found / too long / IPC down)
//!   `outlet_write_retained` → 0 success, `EINVAL` payload refused (OUT-08),
//!                             `ESTALE` stale/never-issued handle or IPC down
//!                             (`ApiError::Unavailable`), -1 OOB arguments
//!   `outlet_list_len`       → count ≥ 0, or `ESTALE` (server unreachable or
//!                             predates outlets)
//!   `outlet_list_entry`     → bytes written ≥ 0, -1 out of range / OOB / IPC down

use std::future::Future;
use std::sync::Arc;

use myrmic_common::types::error::{EINVAL, ESTALE};
use wasmtime::{Caller, Linker};

use super::sl_claim::CellIdentity;

use crate::Result;

use super::tap::{read_guest_bytes, read_guest_str, write_guest_bytes, write_guest_i32};

/// Register the four outlet host functions on `linker` using the shared
/// `tap_client` (outlet requests travel over the same socket as tap requests).
///
/// The module namespace is `"outlet"` to match WAMR's
/// `wasm_runtime_register_natives(c"outlet", ...)`.
#[allow(clippy::too_many_lines)]
#[allow(clippy::cast_sign_loss)] // WASM i32 params reinterpreted as pointer/size
#[allow(clippy::cast_possible_wrap)] // u32 → i32 for WASM return values
#[allow(clippy::cast_possible_truncation)] // usize → i32 for byte counts
#[allow(clippy::needless_pass_by_value)] // Arc cloned into closures; by-value is intentional
pub fn link_outlet_functions<S: CellIdentity + Send + 'static>(
    linker: &mut Linker<S>,
    tap_client: Arc<signal_layer_ipc::TapClient>,
) -> Result<()> {
    // ── outlet_resolve(name_ptr: i32, name_len: i32) -> i32 ──────────────────
    // Returns a virtual handle ≥ 1 on success, or -1 if the name is too long /
    // not found / IPC down.
    {
        let client = Arc::clone(&tap_client);
        linker.func_wrap_async(
            "outlet",
            "outlet_resolve",
            move |mut caller: Caller<'_, S>, (name_ptr, name_len): (i32, i32)| {
                let client = Arc::clone(&client);
                let __denied = super::sl_claim::gate(caller.data()).err();
                Box::new(async move {
                    if let Some(code) = __denied {
                        return code;
                    }
                    // Reject a negative or over-long name_len before it is used
                    // to allocate host memory or to build a request frame (same
                    // guard as tap_resolve — the connection is shared).
                    if name_len < 0 || name_len as usize > signal_layer_ipc::MAX_RESOLVE_NAME_LEN {
                        return -1i32;
                    }
                    let Some(name) =
                        read_guest_str(&mut caller, name_ptr as usize, name_len as usize)
                    else {
                        return -1i32;
                    };
                    match client.outlet_resolve(&name).await {
                        Some(vh) => vh as i32,
                        None => -1,
                    }
                }) as Box<dyn Future<Output = i32> + Send>
            },
        )?;
    }

    // ── outlet_write_retained(handle: i32, buf_ptr: i32, buf_len: i32) -> i32 ─
    // Returns 0 on success, EINVAL if the server refuses the payload (it does
    // not decode into the outlet's declared type, OUT-08), ESTALE if the handle
    // is stale/never-issued or IPC is down, -1 for OOB arguments.
    {
        let client = Arc::clone(&tap_client);
        linker.func_wrap_async(
            "outlet",
            "outlet_write_retained",
            move |mut caller: Caller<'_, S>, (handle, buf_ptr, buf_len): (i32, i32, i32)| {
                let client = Arc::clone(&client);
                let __denied = super::sl_claim::gate(caller.data()).err();
                Box::new(async move {
                    use signal_layer_ipc::ClientWrite;
                    if let Some(code) = __denied {
                        return code;
                    }
                    // Reject negative or frame-endangering lengths before any
                    // cast to usize or host allocation.
                    if buf_len < 0 || buf_len as usize > signal_layer_ipc::MAX_OUTLET_WRITE_LEN {
                        return -1i32;
                    }
                    let Some(bytes) =
                        read_guest_bytes(&mut caller, buf_ptr as usize, buf_len as usize)
                    else {
                        return -1i32;
                    };
                    match client.outlet_write(handle as u32, bytes).await {
                        ClientWrite::Ok => 0,
                        ClientWrite::Rejected => EINVAL,
                        ClientWrite::Unavailable => ESTALE,
                    }
                }) as Box<dyn Future<Output = i32> + Send>
            },
        )?;
    }

    // ── outlet_list_len() -> i32 ──────────────────────────────────────────────
    // Returns the outlet count, or ESTALE if the server is unreachable.
    {
        let client = Arc::clone(&tap_client);
        linker.func_wrap_async(
            "outlet",
            "outlet_list_len",
            move |caller: Caller<'_, S>, (): ()| {
                let client = Arc::clone(&client);
                let __denied = super::sl_claim::gate(caller.data()).err();
                Box::new(async move {
                    if let Some(code) = __denied {
                        return code;
                    }
                    match client.outlet_list_len().await {
                        Some(count) => count as i32,
                        // None means unreachable, timed out, or a server that
                        // predates outlets — all unavailable-shaped.
                        None => ESTALE,
                    }
                }) as Box<dyn Future<Output = i32> + Send>
            },
        )?;
    }

    // ── outlet_list_entry(index: i32, name_ptr: i32, name_len: i32, out_kind_ptr: i32) -> i32 ─
    // out_kind_ptr is a guest *mut i32; the wire kind is u8, widened to i32.
    // Returns bytes written, -1 if out of range, OOB, or IPC down.
    {
        let client = Arc::clone(&tap_client);
        linker.func_wrap_async(
            "outlet",
            "outlet_list_entry",
            move |mut caller: Caller<'_, S>,
                  (index, name_ptr, name_len, out_kind_ptr): (i32, i32, i32, i32)| {
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
                    match client.outlet_list_entry(index as u32).await {
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
                            if out_kind_ptr != 0
                                && write_guest_i32(
                                    &mut caller,
                                    out_kind_ptr as usize,
                                    i32::from(kind),
                                )
                                .is_err()
                            {
                                return -1;
                            }
                            n as i32
                        }
                        None => -1,
                    }
                }) as Box<dyn Future<Output = i32> + Send>
            },
        )?;
    }

    // ── outlet_type_id(handle: i32, out_id_ptr: i32) -> i32 ──────────────────
    // out_id_ptr is a guest *mut u32 (little-endian). Returns 0 on success,
    // -1 for a stale/unknown handle, OOB pointer, or IPC down. Same ABI as the
    // WAMR host (swarm#1315); the SDK calls it once at resolve time.
    {
        let client = Arc::clone(&tap_client);
        linker.func_wrap_async(
            "outlet",
            "outlet_type_id",
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
                    match client.outlet_type_id(handle as u32).await {
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
