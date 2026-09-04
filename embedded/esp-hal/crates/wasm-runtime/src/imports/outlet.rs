//! "outlet" module — Host calls for writing Signal Layer outlet slots from WASM.
//!
//! An outlet is the write-side mirror of a tap: a named, typed slot that a WASM
//! cell (or an in-layer step) drives and a backing driver consumes. Writes are
//! non-blocking and access the [`OutletRegistry`] directly from the WAMR thread;
//! the last write wins (no arbitration — single-writer ownership is resolved
//! statically at manifest/codegen time, F1).
//!
//! Command payloads are postcard-encoded and validated against the slot's
//! declared type on decode (OUT-08). Output-mode and allowed-range enforcement
//! live in the backing driver, not here.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::pin::Pin;

use embassy_time::Instant;
use myrmic_sdk::{EINVAL, GENERIC_ERROR, SUCCESS};
use signal_layer_core::{OutletRegistry, Timestamp};
use wamr_rust_sdk::sys as bindings;
use wamr_rust_sdk::sys::NativeSymbol;

use crate::Error;
use crate::macros::{host_function, host_function_decl};

// `OUTLET_REGISTRY` mirrors `TAP_REGISTRY`: written exactly once by `init()`,
// which is called exclusively from the generated
// `pipeline_config::setup_outlet_registry()` before WAMR starts. All host
// functions that access it run on the single WAMR thread after init completes,
// so no synchronisation is required.
static mut OUTLET_REGISTRY: Option<OutletRegistry> = None;

/// Install the outlet registry. Called exclusively from the generated
/// `pipeline_config::setup_outlet_registry()`; must complete before WAMR starts.
#[cfg(feature = "signal-layer")]
pub(crate) fn init(registry: OutletRegistry) {
    // SAFETY: see `OUTLET_REGISTRY` comment — single writer before WAMR, single-threaded reads after.
    unsafe { OUTLET_REGISTRY = Some(registry) };
}

fn registry() -> Option<&'static OutletRegistry> {
    let raw_ptr = core::ptr::addr_of!(OUTLET_REGISTRY);
    // SAFETY: see module-level invariant — write-once before WAMR, read-only after.
    unsafe { (*raw_ptr).as_ref() }
}

/// Register the "outlet" native symbols with WAMR.
#[expect(
    clippy::box_collection,
    reason = "Need to be able to pin from the beginning of the declaration"
)]
pub(crate) fn setup() -> Result<Pin<Box<Vec<NativeSymbol>>>, Error> {
    let native_symbols = Box::pin(vec![
        host_function_decl!(outlet_resolve, c"(*~)i"), // (name_ptr, name_len) -> handle
        host_function_decl!(outlet_write_retained, c"(i*~)i"), // (handle, buf_ptr, buf_len) -> status
        host_function_decl!(outlet_list_len, c"()i"),          // () -> count
        host_function_decl!(outlet_list_entry, c"(i*~*~)i"), // (index, name_ptr, name_len, out_kind_ptr, out_kind_len) -> bytes_written
        host_function_decl!(outlet_type_id, c"(i*~)i"), // (handle, out_id_ptr, out_id_len) -> status
    ]);

    // safety: C FFI
    let success = unsafe {
        bindings::wasm_runtime_register_natives(
            c"outlet".as_ptr(),
            native_symbols.as_ptr().cast_mut(),
            native_symbols.len() as u32,
        )
    };

    if success {
        log::info!(
            "[wasm] {} symbol(s) registered for module 'outlet'",
            native_symbols.len()
        );
        Ok(native_symbols)
    } else {
        Err(Error::Import)
    }
}

/// `outlet_resolve(name_ptr, name_len) -> handle`
#[host_function]
fn outlet_resolve(name_ptr: *const u8, name_len: i32) -> i32 {
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
        log::error!("[wasm] outlet_resolve: registry not initialised");
        return GENERIC_ERROR;
    };

    match registry.resolve(name) {
        #[expect(
            clippy::expect_used,
            reason = "The WASM Host Function contract has to change"
        )]
        Some(handle) => handle.try_into().expect("handle ID too big to fit in i32"),
        None => {
            log::warn!("[wasm] outlet_resolve: unknown outlet '{}'", name);
            -1
        }
    }
}

/// `outlet_write_retained(handle, buf_ptr, buf_len) -> status`
///
/// Writes the postcard-encoded command in `buf_ptr[0..buf_len]` into the outlet
/// as its latest value, stamped with the host monotonic clock. Returns
/// [`SUCCESS`] (0) on success, [`EINVAL`] if the payload does not decode into
/// the slot's declared type, or [`GENERIC_ERROR`] on a null pointer, bad handle,
/// uninitialised registry, or wrong slot kind.
#[host_function]
fn outlet_write_retained(handle: i32, buf_ptr: *const u8, buf_len: i32) -> i32 {
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
    let Some(outlet) = registry.get(handle as u32) else {
        return GENERIC_ERROR;
    };

    #[expect(
        clippy::cast_sign_loss,
        reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
    )]
    // SAFETY: WAMR validates *~ (ptr, len) pairs against linear memory bounds before
    // dispatch; lengths come from slice.len() as c_int on the SDK side — always non-negative.
    let bytes = unsafe { core::slice::from_raw_parts(buf_ptr, buf_len as usize) };

    let ts = Timestamp(Instant::now().as_millis());
    match outlet.write_bytes(ts, bytes) {
        Ok(()) => SUCCESS,
        Err(signal_layer_core::TapError::Decode) => EINVAL,
        Err(_) => GENERIC_ERROR,
    }
}

/// `outlet_list_len() -> count`
///
/// Returns the number of registered outlets, or `GENERIC_ERROR` if the registry
/// is uninitialised.
#[host_function]
fn outlet_list_len() -> i32 {
    match registry() {
        #[expect(
            clippy::expect_used,
            reason = "The WASM Host Function contract has to change"
        )]
        Some(r) => r
            .len()
            .try_into()
            .expect("too many registered outlets to fit in i32"),
        None => {
            log::error!("[wasm] outlet_list_len: registry not initialised");
            GENERIC_ERROR
        }
    }
}

/// `outlet_list_entry(index, name_ptr, name_len, out_kind_ptr, out_kind_len) -> bytes_written`
///
/// Writes the outlet name at `index` into `name_ptr[0..name_len]` (truncated to
/// fit). If `name_ptr` is null the name is not written but the required byte
/// count is still returned. `out_kind_ptr`/`out_kind_len` are a WAMR `*~`
/// pointer/length pair: if `out_kind_ptr` is non-null the little-endian `i32` kind
/// (0=retained) is written into `out_kind_ptr[0..4]`, which requires
/// `out_kind_len == 4`.
///
/// Returns the number of bytes written (or that would be written), -1 if `index`
/// is out of bounds, `EINVAL` if a non-null `out_kind` buffer is not exactly 4
/// bytes, or `GENERIC_ERROR` on other errors.
#[host_function]
fn outlet_list_entry(
    index: i32,
    name_ptr: *mut u8,
    name_len: i32,
    out_kind_ptr: *mut u8,
    out_kind_len: i32,
) -> i32 {
    let Some(registry) = registry() else {
        log::error!("[wasm] outlet_list_entry: registry not initialised");
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
    let Some(outlet) = registry.get(index as u32) else {
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
        let kind = outlet.kind() as i32;
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

/// `outlet_type_id(handle, out_id_ptr, out_id_len) -> status`
///
/// Writes the outlet's declared command type ([`signal_layer_core::WireType`]
/// `TYPE_ID`) into `*out_id_ptr`. Returns [`SUCCESS`] (0), or
/// [`GENERIC_ERROR`] on a null pointer, unknown handle, or uninitialised
/// registry. The SDK fetches this once at resolve time and refuses a
/// wrong-typed `write_typed` before it reaches the registry (swarm#1315).
#[host_function]
fn outlet_type_id(handle: i32, out_id_ptr: *mut u8, out_id_len: i32) -> i32 {
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
        log::error!("[wasm] outlet_type_id: registry not initialised");
        return GENERIC_ERROR;
    };

    #[expect(
        clippy::cast_sign_loss,
        reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
    )]
    let Some(outlet) = registry.get(handle as u32) else {
        return GENERIC_ERROR;
    };

    // SAFETY: WAMR's `*~` validated `[out_id_ptr, out_id_ptr + out_id_len)` against
    // linear-memory bounds before dispatch; the check above guarantees a 4-byte
    // buffer, and the byte copy imposes no alignment requirement.
    let out_id = unsafe { core::slice::from_raw_parts_mut(out_id_ptr, out_id_len) };
    out_id[..size_of::<u32>()].copy_from_slice(&outlet.wire_type_id().to_le_bytes());
    SUCCESS
}
