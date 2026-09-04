//! "tap" module — Host calls for reading Signal Layer tap slots from WASM.
//!
//! Tap reads are non-blocking and access the [`TapRegistry`] directly from the
//! WAMR thread. This is correct because all reads are from pre-computed
//! in-memory slots — no async I/O needed.
//!
//! "Tap" is the unified name for all named data slots (retained, event, batch).

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::pin::Pin;

use myrmic_sdk::{EINVAL, GENERIC_ERROR, SUCCESS};
use signal_layer_core::{SlotEntry, TapRegistry};
use wamr_rust_sdk::sys as bindings;
use wamr_rust_sdk::sys::NativeSymbol;

use crate::Error;
use crate::macros::{host_function, host_function_decl};

// `TAP_REGISTRY` is written exactly once by `init()`, which is called
// exclusively from `pipeline_config::setup_tap_registry()` (generated code) —
// there is no other call site. All WAMR host functions that read it run on the
// single WAMR thread after init completes. No concurrent write is possible, so
// no synchronisation is required.
static mut TAP_REGISTRY: Option<TapRegistry> = None;

/// Install the tap registry. Called exclusively from the generated
/// `pipeline_config::setup_tap_registry()`; must complete before WAMR starts.
#[cfg(feature = "signal-layer")]
pub(crate) fn init(registry: TapRegistry) {
    // SAFETY: see `TAP_REGISTRY` comment — single writer before WAMR, single-threaded reads after.
    unsafe { TAP_REGISTRY = Some(registry) };
}

fn registry() -> Option<&'static TapRegistry> {
    let raw_ptr = core::ptr::addr_of!(TAP_REGISTRY);
    // SAFETY: see module-level invariant — write-once before WAMR, read-only after.
    unsafe { (*raw_ptr).as_ref() }
}

/// Register the "tap" native symbols with WAMR.
#[expect(
    clippy::box_collection,
    reason = "Need to be able to pin from the beginning of the declaration"
)]
pub(crate) fn setup() -> Result<Pin<Box<Vec<NativeSymbol>>>, Error> {
    let native_symbols = Box::pin(vec![
        host_function_decl!(tap_resolve, c"(*~)i"), // (name_ptr, name_len) -> handle
        host_function_decl!(tap_read_retained, c"(i*~*~)i"), // (handle, buf_ptr, buf_len, ts_out_ptr, ts_out_len) -> bytes
        host_function_decl!(tap_take_event, c"(i*~)i"),      // (handle, buf_ptr, buf_len) -> bytes
        host_function_decl!(tap_drain_batch, c"(i*~)i"),     // (handle, buf_ptr, buf_len) -> bytes
        host_function_decl!(tap_list_len, c"()i"),           // () -> count
        host_function_decl!(tap_list_entry, c"(i*~*~)i"), // (index, name_ptr, name_len, out_kind_ptr, out_kind_len) -> bytes_written
        host_function_decl!(tap_type_id, c"(i*~)i"), // (handle, out_id_ptr, out_id_len) -> status
    ]);

    // safety: C FFI
    let success = unsafe {
        bindings::wasm_runtime_register_natives(
            c"tap".as_ptr(),
            native_symbols.as_ptr().cast_mut(),
            native_symbols.len() as u32,
        )
    };

    if success {
        log::info!(
            "[wasm] {} symbol(s) registered for module 'tap'",
            native_symbols.len()
        );
        Ok(native_symbols)
    } else {
        Err(Error::Import)
    }
}

/// `tap_resolve(name_ptr, name_len) -> handle`
#[host_function]
fn tap_resolve(name_ptr: *const u8, name_len: i32) -> i32 {
    if name_ptr.is_null() {
        log::info!("name pointer is null");
        return GENERIC_ERROR;
    }

    #[expect(
        clippy::cast_sign_loss,
        reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
    )]
    // SAFETY: WAMR validates *~ (ptr, len) pairs against linear memory bounds before
    // dispatch; lengths come from slice.len() as c_int on the SDK side — always non-negative.
    let name = unsafe { core::slice::from_raw_parts(name_ptr, name_len as usize) };
    let Ok(name) = core::str::from_utf8(name) else {
        return GENERIC_ERROR;
    };

    let Some(registry) = registry() else {
        log::error!("[wasm] tap_resolve: registry not initialised");
        return GENERIC_ERROR;
    };

    match registry.resolve(name) {
        #[expect(
            clippy::expect_used,
            reason = "The WASM Host Function contract has to change"
        )]
        Some(handle) => handle.try_into().expect("handle ID too big to fit in i32"),
        None => {
            log::warn!("[wasm] tap_resolve: unknown tap '{}'", name);
            -1
        }
    }
}

/// `tap_read_retained(handle, buf_ptr, buf_len, ts_out_ptr, ts_out_len) -> bytes_written`
#[host_function]
fn tap_read_retained(
    handle: i32,
    buf_ptr: *mut u8,
    buf_len: i32,
    ts_out_ptr: *mut u8,
    ts_out_len: i32,
) -> i32 {
    if buf_ptr.is_null() {
        log::info!("buffer pointer is null");
        return GENERIC_ERROR;
    }

    let Some(registry) = registry() else {
        return GENERIC_ERROR;
    };

    #[expect(
        clippy::cast_sign_loss,
        reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
    )]
    let Some(slot) = registry.get(handle as u32) else {
        return GENERIC_ERROR;
    };

    match slot {
        SlotEntry::Retained(retained) => {
            #[expect(
                clippy::cast_sign_loss,
                reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
            )]
            // SAFETY: WAMR validates *~ (ptr, len) pairs against linear memory bounds before
            // dispatch; lengths come from slice.len() as c_int on the SDK side — always non-negative.
            let buf = unsafe { core::slice::from_raw_parts_mut(buf_ptr, buf_len as usize) };
            let mut ts = 0u64;
            match retained.read_bytes(&mut ts, buf) {
                Ok(n) => {
                    if !ts_out_ptr.is_null() {
                        #[expect(
                            clippy::cast_sign_loss,
                            reason = "WAMR host function: WASM i32 length reinterpreted as size"
                        )]
                        let ts_out_len = ts_out_len as usize;
                        if ts_out_len != size_of::<u64>() {
                            return EINVAL;
                        }
                        // SAFETY: WAMR's `*~` validated `[ts_out_ptr, ts_out_ptr + ts_out_len)`
                        // against linear-memory bounds before dispatch; the check above
                        // guarantees the buffer is exactly the 8-byte timestamp, and the
                        // little-endian byte copy imposes no alignment requirement on `ts_out_ptr`.
                        let ts_out =
                            unsafe { core::slice::from_raw_parts_mut(ts_out_ptr, ts_out_len) };
                        ts_out[..size_of::<u64>()].copy_from_slice(&ts.to_le_bytes());
                    }
                    #[expect(
                        clippy::expect_used,
                        reason = "The WASM Host Function contract has to change"
                    )]
                    n.try_into().expect("too many bytes to fit in i32")
                }
                Err(signal_layer_core::TapError::Empty) => 0,
                Err(signal_layer_core::TapError::BufferTooSmall) => EINVAL,
                Err(_) => GENERIC_ERROR,
            }
        }
        _ => GENERIC_ERROR, // wrong slot kind
    }
}

/// `tap_take_event(handle, buf_ptr, buf_len) -> bytes_written`
#[host_function]
fn tap_take_event(handle: i32, buf_ptr: *mut u8, buf_len: i32) -> i32 {
    if buf_ptr.is_null() {
        log::info!("buffer pointer is null");
        return GENERIC_ERROR;
    }

    let Some(registry) = registry() else {
        return GENERIC_ERROR;
    };

    #[expect(
        clippy::cast_sign_loss,
        reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
    )]
    let Some(slot) = registry.get(handle as u32) else {
        return GENERIC_ERROR;
    };

    match slot {
        SlotEntry::Event(event) => {
            #[expect(
                clippy::cast_sign_loss,
                reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
            )]
            // SAFETY: WAMR validates *~ (ptr, len) pairs against linear memory bounds before
            // dispatch; lengths come from slice.len() as c_int on the SDK side — always non-negative.
            let buf = unsafe { core::slice::from_raw_parts_mut(buf_ptr, buf_len as usize) };
            match event.take_bytes(buf) {
                #[expect(
                    clippy::expect_used,
                    reason = "The WASM Host Function contract has to change"
                )]
                Ok(n) => n.try_into().expect("too many bytes to fit in i32"),
                Err(signal_layer_core::TapError::Empty) => 0,
                Err(signal_layer_core::TapError::BufferTooSmall) => EINVAL,
                Err(_) => GENERIC_ERROR,
            }
        }
        _ => GENERIC_ERROR, // wrong slot kind
    }
}

/// `tap_drain_batch(handle, buf_ptr, buf_len) -> bytes_written`
#[host_function]
fn tap_drain_batch(_handle: i32, _buf_ptr: *mut u8, _buf_len: i32) -> i32 {
    // BatchSlot codegen is not yet implemented; this host function is registered
    // so the WASM ABI is stable, but it always returns 0 frames drained.
    0
}

/// `tap_list_len() -> count`
///
/// Returns the number of registered taps, or `GENERIC_ERROR` if the registry is uninitialised.
#[host_function]
fn tap_list_len() -> i32 {
    match registry() {
        #[expect(
            clippy::expect_used,
            reason = "The WASM Host Function contract has to change"
        )]
        Some(r) => r
            .len()
            .try_into()
            .expect("too many registered taps to fit in i32"),
        None => {
            log::error!("[wasm] tap_list_len: registry not initialised");
            GENERIC_ERROR
        }
    }
}

/// `tap_list_entry(index, name_ptr, name_len, out_kind_ptr, out_kind_len) -> bytes_written`
///
/// Writes the tap name at `index` into `name_ptr[0..name_len]` (truncated to fit).
/// If `name_ptr` is null the name is not written but the required byte count is still returned.
/// `out_kind_ptr`/`out_kind_len` are a WAMR `*~` pointer/length pair: if `out_kind_ptr` is
/// non-null the little-endian `i32` kind (0=retained, 1=event, 2=batch) is written into
/// `out_kind_ptr[0..4]`, which requires `out_kind_len == 4`.
///
/// Returns the number of bytes written (or that would be written), -1 if `index` is out of
/// bounds, `EINVAL` if a non-null `out_kind` buffer is not exactly 4 bytes, or `GENERIC_ERROR`
/// on other errors.
#[host_function]
fn tap_list_entry(
    index: i32,
    name_ptr: *mut u8,
    name_len: i32,
    out_kind_ptr: *mut u8,
    out_kind_len: i32,
) -> i32 {
    let Some(registry) = registry() else {
        log::error!("[wasm] tap_list_entry: registry not initialised");
        return GENERIC_ERROR;
    };

    #[expect(
        clippy::cast_sign_loss,
        reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
    )]
    let Some(name) = registry.name_at(index as u32) else {
        return -1; // index out of bounds
    };

    #[expect(
        clippy::cast_sign_loss,
        reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
    )]
    let Some(slot) = registry.get(index as u32) else {
        return -1;
    };

    if !out_kind_ptr.is_null() {
        #[expect(
            clippy::cast_sign_loss,
            reason = "WAMR host function: WASM i32 length reinterpreted as size"
        )]
        let out_kind_len = out_kind_len as usize;
        if out_kind_len != size_of::<i32>() {
            return EINVAL;
        }
        let kind = slot.kind() as i32;
        // SAFETY: WAMR's `*~` validated `[out_kind_ptr, out_kind_ptr + out_kind_len)` against
        // linear-memory bounds before dispatch; the check above guarantees the buffer is
        // exactly the 4-byte kind, and the byte copy imposes no alignment requirement.
        let out_kind = unsafe { core::slice::from_raw_parts_mut(out_kind_ptr, out_kind_len) };
        out_kind[..size_of::<i32>()].copy_from_slice(&kind.to_le_bytes());
    }

    let bytes = name.as_bytes();
    if !name_ptr.is_null() && name_len > 0 {
        #[expect(
            clippy::cast_sign_loss,
            reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
        )]
        // SAFETY: WAMR validates *~ (ptr, len) pairs against linear memory bounds before
        // dispatch; name_len > 0 is checked above and lengths are always non-negative.
        let dst = unsafe { core::slice::from_raw_parts_mut(name_ptr, name_len as usize) };
        let written = bytes.len().min(dst.len());
        dst[..written].copy_from_slice(&bytes[..written]);
        #[expect(
            clippy::expect_used,
            reason = "The WASM Host Function contract has to change"
        )]
        written
            .try_into()
            .expect("written is too long to fit in i32")
    } else {
        #[expect(
            clippy::expect_used,
            reason = "The WASM Host Function contract has to change"
        )]
        bytes
            .len()
            .try_into()
            .expect("buf is too long to fit in i32")
    }
}

/// `tap_type_id(handle, out_id_ptr, out_id_len) -> status`
///
/// Writes the slot's declared wire type ([`signal_layer_core::WireType`]
/// `TYPE_ID`) into `*out_id_ptr`. Returns [`SUCCESS`] (0), or [`GENERIC_ERROR`]
/// on a null pointer, unknown handle, or uninitialised registry. The SDK
/// fetches this once at resolve time and checks typed reads against it
/// (swarm#1315).
#[host_function]
fn tap_type_id(handle: i32, out_id_ptr: *mut u8, out_id_len: i32) -> i32 {
    if out_id_ptr.is_null() {
        return GENERIC_ERROR;
    }
    #[expect(
        clippy::cast_sign_loss,
        reason = "WAMR host function: WASM i32 length reinterpreted as size"
    )]
    let out_id_len = out_id_len as usize;
    if out_id_len != size_of::<u32>() {
        return EINVAL;
    }

    let Some(registry) = registry() else {
        log::error!("[wasm] tap_type_id: registry not initialised");
        return GENERIC_ERROR;
    };

    #[expect(
        clippy::cast_sign_loss,
        reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
    )]
    let Some(entry) = registry.get(handle as u32) else {
        return GENERIC_ERROR;
    };

    // SAFETY: WAMR's `*~` validated `[out_id_ptr, out_id_ptr + out_id_len)` against
    // linear-memory bounds before dispatch; the check above guarantees a 4-byte
    // buffer, and the byte copy imposes no alignment requirement.
    let out_id = unsafe { core::slice::from_raw_parts_mut(out_id_ptr, out_id_len) };
    out_id[..size_of::<u32>()].copy_from_slice(&entry.wire_type_id().to_le_bytes());
    SUCCESS
}
