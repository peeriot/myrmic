//! WASM `db` imports
//!
//! Exposes the datalayer host functions (key-value, table, time-series and semantic
//! operations) to guest cell modules. Each host function decodes the `myrmic_common`
//! request from guest memory, joins it to the application of the cell function that
//! called it — [`defer`] for a write nothing reads back, [`apply`] for anything
//! returning a value — and writes the response back into guest memory.

mod blob;
mod kv;
mod sem;
mod tb;
mod ts;

// Make sure the generated `#[host_function]` exports are visible to the setup in this file
#[expect(clippy::wildcard_imports, reason = "Needed to help proc macros")]
use blob::*;
#[expect(clippy::wildcard_imports, reason = "Needed to help proc macros")]
use kv::*;
#[expect(clippy::wildcard_imports, reason = "Needed to help proc macros")]
use sem::*;
#[expect(clippy::wildcard_imports, reason = "Needed to help proc macros")]
use tb::*;
#[expect(clippy::wildcard_imports, reason = "Needed to help proc macros")]
use ts::*;

use alloc::boxed::Box;
use alloc::format;
use alloc::vec;
use alloc::vec::Vec;
use core::ffi::c_int;
use core::pin::Pin;

use cell_protocol::{GATEWAY_ASSETS_DB, NAMESPACE_CELLS, NAMESPACE_GATEWAY, scope_of_cell};
use db_client::v1::models::{Deferrable, Operation, Scope as DbScope};
use myrmic_common::db::{Namespace, Scope as WasmScope};
use myrmic_common::types::error::{EINVAL, EPERM, GENERIC_ERROR, SUCCESS};
use wamr_rust_sdk::sys;
use wamr_rust_sdk::sys::NativeSymbol;

use crate::async_request::cell_host::CellHost;
use crate::async_request::db::DbClient;
use crate::async_request::send_request_and_wait;
use crate::{Error, host_function_decl};

/// Sets up the `db` imports
#[expect(
    clippy::box_collection,
    reason = "Need to be able to pin from the beginning of the declaration"
)]
pub(crate) fn setup() -> Result<Pin<Box<Vec<NativeSymbol>>>, Error> {
    let native_symbols = Box::pin(vec![
        // Key-value
        host_function_decl!(key_put, c"(*~)i"), // (req ptr + len) -> i32
        host_function_decl!(key_delete, c"(*~)i"), // (req ptr + len) -> i32
        host_function_decl!(key_get, c"(*~*~)i"), // (req ptr + len + rsp ptr + len) -> i32
        host_function_decl!(key_prefix, c"(*~*~)i"), // (req ptr + len + rsp ptr + len) -> i32
        // Table
        host_function_decl!(tb_insert, c"(*~*~)i"), // (req ptr + len + rsp ptr + len) -> i32
        host_function_decl!(tb_count, c"(*~*~)i"),  // (req ptr + len + rsp ptr + len) -> i32
        host_function_decl!(tb_get, c"(*~*~)i"),    // (req ptr + len + rsp ptr + len) -> i32
        host_function_decl!(tb_list, c"(*~*~)i"),   // (req ptr + len + rsp ptr + len) -> i32
        host_function_decl!(tb_append, c"(*~)i"),   // (req ptr + len) -> i32
        host_function_decl!(tb_delete, c"(*~)i"),   // (req ptr + len) -> i32
        // Blob
        host_function_decl!(blob_store, c"(*~*~*~)i"), // (req + blob + rsp ptr/len) -> i32
        host_function_decl!(blob_link, c"(*~)i"),      // (req ptr + len) -> i32
        host_function_decl!(blob_unlink, c"(*~)i"),    // (req ptr + len) -> i32
        host_function_decl!(blob_move, c"(*~)i"),      // (req ptr + len) -> i32
        host_function_decl!(blob_resolve, c"(*~*~)i"), // (req ptr + len + rsp ptr + len) -> i32
        host_function_decl!(path_resolve, c"(*~*~)i"), // (req ptr + len + rsp ptr + len) -> i32
        host_function_decl!(paths_list, c"(*~*~)i"),   // (req ptr + len + rsp ptr + len) -> i32
        // Time series
        host_function_decl!(publish_measurement, c"(*~)i"), // (req ptr + len) -> i32
        host_function_decl!(find_measurement, c"(*~*~)i"), // (req ptr + len + rsp ptr + len) -> i32
        // Semantic
        host_function_decl!(sem_update, c"(*~)i"), // (req ptr + len) -> i32
        host_function_decl!(sem_select, c"(*~*~)i"), // (req ptr + len + rsp ptr + len) -> i32
    ]);

    // safety: C FFI
    let success = unsafe {
        sys::wasm_runtime_register_natives(
            c"db".as_ptr(),
            native_symbols.as_ptr().cast_mut(),
            native_symbols.len() as u32,
        )
    };

    if success {
        Ok(native_symbols)
    } else {
        Err(Error::Import)
    }
}

/// Maps a guest `Scope` onto a datalayer scope.
///
/// Mirrors the `sorg-execution` host: a cell's private data is its own
/// database under the cells namespace ([`scope_of_cell`]), reserved public
/// namespaces are refused with [`EPERM`], and empty segments with [`EINVAL`].
/// Optional overrides default to `d` / `p`.
fn transform_scope(wasm_scope: WasmScope) -> Result<DbScope, c_int> {
    let (ns, database, schema) = wasm_scope.into_inner();

    let mut scope = match ns {
        // The cell's own database under the cells namespace, stamped by the
        // host — the guest never names it (and can't pick another database).
        Namespace::Private => scope_of_cell(send_request_and_wait(CellHost::GetSri)),
        Namespace::Public(public_ns) => {
            if is_reserved_namespace(public_ns.as_ref()) {
                return Err(EPERM);
            }

            let mut scope = DbScope {
                namespace: non_empty(public_ns)?,
                ..Default::default()
            };

            if let Some(database) = database {
                scope.database = non_empty(database)?;
            }

            scope
        }
    };

    if let Some(schema) = schema {
        scope.schema = non_empty(schema)?;
    }

    // A cell owns exactly one scope in the gateway namespace: its own assets.
    // Everything else there — the routing table, another cell's assets — is the
    // gateway's. The identity comes from the host, never from the guest.
    // Only asked for when the namespace is actually the gateway's, since it
    // costs a host round-trip.
    if scope.namespace == NAMESPACE_GATEWAY {
        let sri = send_request_and_wait(CellHost::GetSri);

        if scope.database != GATEWAY_ASSETS_DB || scope.schema != format!("{sri}") {
            return Err(EPERM);
        }
    }

    Ok(scope)
}

/// Makes sure the cell can access the provided scope.
///
/// * `sys`, `sorg` and `tele` (system state, cell/execution metadata, telemetry
///   `swarm_telemetry::db::NAMESPACE_TELE`) are the system's alone. `sorg` holds
///   the registries, placements and leases supervision trusts, so a cell that
///   could write there could forge its own lineage.
/// * the `CELLS` namespace holds everything cell-owned — each cell's private
///   data, mailbox and event bus, keyed by its SRI as the database. A cell
///   reaches its own slice through `Namespace::Private`, so naming `CELLS`
///   directly is refused.
fn is_reserved_namespace(ns: &str) -> bool {
    ns == "sys" || ns == "sorg" || ns == "tele" || ns == NAMESPACE_CELLS
}

/// A segment the guest named explicitly must be non-empty — `Some("")` is a
/// guest bug, not a request for the default.
fn non_empty(segment: alloc::borrow::Cow<'static, str>) -> Result<alloc::string::String, c_int> {
    if segment.is_empty() {
        return Err(EINVAL);
    }

    Ok(segment.into_owned())
}

/// Inverse of [`transform_scope`], for scopes handed back to the guest inside a
/// `BlobId`, so the id re-resolves to the same place when the guest passes it to
/// a later call. The calling cell's own slice comes back as private (the cells
/// namespace is reserved, so a public scope naming it would be refused);
/// everything else as an explicit public namespace. The identity is only asked
/// for when the namespace could be the cell's own, since it costs a host
/// round-trip.
fn untransform_scope(scope: DbScope) -> WasmScope {
    if scope.namespace == NAMESPACE_CELLS {
        let sri = send_request_and_wait(CellHost::GetSri);

        if scope.database == format!("{sri}") {
            return WasmScope::private_owned(Some(scope.schema));
        }
    }

    WasmScope::public_owned(scope.namespace, Some(scope.database), Some(scope.schema))
}

/// Buffers a write the guest gets nothing back from. It applies with whatever
/// the function does next — or, if the guest asks for nothing else, in the one
/// round trip that commits.
///
/// Returns the guest's status directly: buffering is refused once an earlier
/// operation has aborted the function's transaction, and the guest has to hear
/// that rather than a success for a write that can never commit.
pub(super) fn defer<T: Deferrable>(op: T) -> c_int {
    match send_request_and_wait(DbClient::Defer(op.into())) {
        Ok(()) => SUCCESS,
        Err(err) => {
            log::error!("[db] host call could not defer: {err}");
            GENERIC_ERROR
        }
    }
}

/// Applies an operation the guest wants the result of, flushing everything
/// deferred before it in one round trip. Failure is reported to the guest as a
/// generic failure, and has already aborted the function's transaction.
pub(super) fn apply<T: Operation>(op: T) -> Result<T::Response, c_int> {
    let applied = send_request_and_wait(DbClient::Apply(op.into())).map_err(|err| {
        log::error!("[db] host call failed: {err}");
        GENERIC_ERROR
    })?;

    T::Response::try_from(applied).map_err(|_| {
        log::error!(
            "[db] application returned the wrong response for {}",
            T::NAME
        );
        GENERIC_ERROR
    })
}

/// Reads and postcard-decodes a request from guest linear memory.
///
/// # Safety
/// `buffer` must point to `length` valid bytes within the calling module's memory.
unsafe fn decode_request<T: serde::de::DeserializeOwned>(
    buffer: *const u8,
    length: c_int,
    context: &str,
) -> Result<T, c_int> {
    if buffer.is_null() {
        log::error!("[db] request buffer pointer is null");
        return Err(GENERIC_ERROR);
    }

    #[expect(
        clippy::cast_sign_loss,
        reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
    )]
    // safety: caller guarantees `buffer` points to `length` readable bytes
    let data = unsafe { core::slice::from_raw_parts(buffer, length as usize) };

    match postcard::take_from_bytes::<T>(data) {
        Ok((value, _rest)) => Ok(value),
        Err(err) => {
            log::error!("[db] failed to deserialize {context}: {err}");
            Err(GENERIC_ERROR)
        }
    }
}

/// Postcard-encodes a response into guest linear memory, returning [`SUCCESS`] or an error code.
///
/// # Safety
/// `buffer` must point to `length` writable bytes within the calling module's memory.
unsafe fn encode_response<T: serde::Serialize>(
    buffer: *mut u8,
    length: c_int,
    value: &T,
    context: &str,
) -> c_int {
    if buffer.is_null() {
        log::error!("[db] response buffer pointer is null");
        return GENERIC_ERROR;
    }

    #[expect(
        clippy::cast_sign_loss,
        reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
    )]
    // safety: caller guarantees `buffer` points to `length` writable bytes
    let data = unsafe { core::slice::from_raw_parts_mut(buffer, length as usize) };

    match postcard::to_slice(value, data) {
        Ok(_) => SUCCESS,
        Err(err) => {
            log::error!("[db] failed to serialize {context}: {err}");
            EINVAL
        }
    }
}
