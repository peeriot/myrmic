use wasmtime::Caller;

use db_client::v1::models::{Deferrable, Operation, Scope as DbScope, TxId};
use myrmic_common::db::Scope as WasmScope;

use crate::wasm::cell::state::CellState;

use cell_protocol::guest_scope::resolve_guest_scope;
use cell_protocol::{NAMESPACE_CELLS, Sri};
use myrmic_common::types::error::{GENERIC_ERROR, SUCCESS};

pub(super) use cell_protocol::guest_scope::key_name;

pub(super) use blob::{
    blob_link, blob_move, blob_resolve, blob_store, blob_unlink, path_resolve, paths_list,
};
pub(super) use kv::{key_delete, key_get, key_prefix, key_put};
pub(super) use sem::{sem_select, sem_update};
pub(super) use tb::{tb_append, tb_count, tb_delete, tb_get, tb_insert, tb_list};
pub(super) use ts::{find_measurement, publish_measurement};

mod blob;
mod kv;
mod sem;
mod tb;
mod ts;

pub(super) fn transform_scope(
    caller: &mut Caller<'_, CellState>,
    wasm_scope: WasmScope,
) -> Result<DbScope, i32> {
    resolve_guest_scope(caller.data().sri(), wasm_scope)
}

/// Inverse of [`transform_scope`], for scopes handed back to the guest inside a
/// `BlobId`, so the id re-resolves to the same place when the guest passes it
/// to a later call. The calling cell's own slice comes back as private (the
/// cells namespace is reserved, so a public scope naming it would be refused);
/// everything else as an explicit public namespace.
pub(super) fn untransform_scope(sri: &Sri, scope: DbScope) -> WasmScope {
    if scope.namespace == NAMESPACE_CELLS && scope.database == sri.to_string() {
        WasmScope::private_owned(Some(scope.schema))
    } else {
        WasmScope::public_owned(scope.namespace, Some(scope.database), Some(scope.schema))
    }
}

/// The transaction the db call joins — the cell function's current one, opened
/// now if this is the function's first db call. Failure to open one is reported
/// to the guest as a generic failure.
pub(super) async fn current_tx(caller: &mut Caller<'_, CellState>) -> Result<TxId, i32> {
    caller.data_mut().transaction().await.map_err(|err| {
        tracing::error!("db host call could not get a transaction: {err}");
        GENERIC_ERROR
    })
}

/// Buffers a write the guest gets nothing back from. It applies with whatever
/// the function does next — or, if the guest asks for nothing else, in the one
/// round trip that commits.
///
/// Returns the guest's status directly: buffering is refused once an earlier
/// operation has aborted the function's transaction, and the guest has to hear
/// that rather than a success for a write that can never commit.
pub(super) fn defer<T: Deferrable>(caller: &mut Caller<'_, CellState>, op: T) -> i32 {
    match caller.data_mut().defer(op) {
        Ok(()) => SUCCESS,
        Err(err) => {
            tracing::error!("db host call could not defer: {err}");
            GENERIC_ERROR
        }
    }
}

/// Applies an operation the guest wants the result of, flushing everything
/// deferred before it in one round trip. Failure is reported to the guest as a
/// generic failure, and has already aborted the function's transaction.
pub(super) async fn apply<T: Operation>(
    caller: &mut Caller<'_, CellState>,
    op: T,
) -> Result<T::Response, i32> {
    caller.data_mut().apply(op).await.map_err(|err| {
        tracing::error!("db host call failed: {err}");
        GENERIC_ERROR
    })
}
