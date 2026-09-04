use wasmtime::Caller;

use db_client::v1::models::{Deferrable, Operation, Scope as DbScope, TxId};
use myrmic_common::db::{Namespace, Scope as WasmScope};

use crate::wasm::cell::state::CellState;

use cell_protocol::{GATEWAY_ASSETS_DB, NAMESPACE_CELLS, NAMESPACE_GATEWAY, Sri, scope_of_cell};
use myrmic_common::types::error::{EINVAL, EPERM, GENERIC_ERROR, SUCCESS};

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
    let (ns, db, schema) = wasm_scope.into_inner();

    let mut scope = match ns {
        // The cell's own database under the cells namespace, stamped by the
        // host — the guest never names it (and can't pick another database).
        Namespace::Private => scope_of_cell(caller.data().sri()),
        Namespace::Public(public_ns) => {
            if is_reserved_namespace(public_ns.as_ref()) {
                return Err(EPERM);
            }

            let mut scope = DbScope {
                namespace: non_empty(public_ns)?,
                ..Default::default()
            };

            if let Some(db) = db {
                scope.database = non_empty(db)?;
            }

            scope
        }
    };

    if let Some(schema) = schema {
        scope.schema = non_empty(schema)?;
    }

    if scope.namespace == NAMESPACE_GATEWAY && !is_own_asset_scope(&scope, caller.data().sri()) {
        return Err(EPERM);
    }

    Ok(scope)
}

/// A segment the guest named explicitly must be non-empty — `Some("")` is a
/// guest bug, not a request for the default.
fn non_empty(segment: std::borrow::Cow<'static, str>) -> Result<String, i32> {
    if segment.is_empty() {
        return Err(EINVAL);
    }

    Ok(segment.into_owned())
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

/// Whether `scope` is the one place in the gateway namespace `sri` owns: its
/// own assets.
///
/// The namespace can't simply be reserved — a cell publishes its own static
/// assets there. Everything else in it belongs to the gateway: the routing
/// table, and every other cell's assets. Checked against the identity the host
/// stamped, never one the guest supplied.
fn is_own_asset_scope(scope: &DbScope, sri: &Sri) -> bool {
    scope.database == GATEWAY_ASSETS_DB && scope.schema == sri.to_string()
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

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::{DbScope, GATEWAY_ASSETS_DB, NAMESPACE_GATEWAY, Sri, is_own_asset_scope};
    use super::{EINVAL, is_reserved_namespace, non_empty};

    fn gateway_scope(database: &str, schema: &str) -> DbScope {
        DbScope {
            namespace: String::from(NAMESPACE_GATEWAY),
            database: String::from(database),
            schema: String::from(schema),
        }
    }

    #[test]
    fn a_cell_owns_its_own_asset_scope() {
        let me = Sri::of_path("chatty").expect("srn");
        let scope = gateway_scope(GATEWAY_ASSETS_DB, &me.to_string());

        assert!(is_own_asset_scope(&scope, &me));
    }

    #[test]
    fn a_cell_cannot_reach_another_cells_assets() {
        let me = Sri::of_path("chatty").expect("srn");
        let other = Sri::of_path("other").expect("srn");
        let scope = gateway_scope(GATEWAY_ASSETS_DB, &other.to_string());

        assert!(!is_own_asset_scope(&scope, &me));
    }

    #[test]
    fn a_cell_cannot_reach_the_routing_table() {
        let me = Sri::of_path("chatty").expect("srn");

        // The routing table, and anything else the gateway owns.
        assert!(!is_own_asset_scope(
            &gateway_scope("gateway-config", "p"),
            &me
        ));
        assert!(!is_own_asset_scope(&gateway_scope("d", "p"), &me));
        // Right database, but not keyed by this cell.
        assert!(!is_own_asset_scope(
            &gateway_scope(GATEWAY_ASSETS_DB, "p"),
            &me
        ));
    }

    #[test]
    fn system_namespaces_are_reserved() {
        assert!(is_reserved_namespace("sys"));
        // Cell/execution metadata: registries, placements, leases.
        assert!(is_reserved_namespace("sorg"));
        assert!(is_reserved_namespace("tele"));
        // The CELLS namespace carries every cell's private data, mailbox and
        // event bus; a cell's own slice is only reachable via Namespace::Private.
        assert!(is_reserved_namespace("CELLS"));
    }

    #[test]
    fn public_namespaces_are_allowed() {
        assert!(!is_reserved_namespace("d"));
        assert!(!is_reserved_namespace("myapp"));
        // A name that merely starts with CELLS is a normal public namespace.
        assert!(!is_reserved_namespace("CELLSISH"));
    }

    #[test]
    fn empty_segments_are_rejected() {
        assert_eq!(non_empty(Cow::Borrowed("")), Err(EINVAL));
        assert_eq!(non_empty(Cow::Borrowed("d")).as_deref(), Ok("d"));
    }
}
