use wasmtime::Caller;

use db_client::v1::models::{
    Deferrable, MAX_KEY_PART_LEN, MAX_SEGMENT_LEN, Operation, Scope as DbScope, TxId, check_segment,
};
use myrmic_common::cells::MAX_NAME_LEN;
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
#[cfg(test)]
mod wiring;

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
                namespace: segment(public_ns)?,
                ..Default::default()
            };

            if let Some(db) = db {
                scope.database = segment(db)?;
            }

            scope
        }
    };

    if let Some(schema) = schema {
        scope.schema = segment(schema)?;
    }

    if scope.namespace == NAMESPACE_GATEWAY && !is_own_asset_scope(&scope, caller.data().sri()) {
        return Err(EPERM);
    }

    Ok(scope)
}

/// A guest-named scope segment: a valid [`key_name`] and [`check_segment`], no `@`.
fn segment(segment: std::borrow::Cow<'static, str>) -> Result<String, i32> {
    key_name(&segment)?;
    if check_segment(&segment).is_err() || segment.contains('@') {
        return Err(EINVAL);
    }

    Ok(segment.into_owned())
}

/// A name the guest hands the db for a key: no NUL, at most [`MAX_KEY_PART_LEN`] bytes.
pub(super) fn key_name(name: &str) -> Result<(), i32> {
    if name.len() > MAX_KEY_PART_LEN || name.contains('\0') {
        return Err(EINVAL);
    }

    Ok(())
}

/// A table entity id the guest chose is part of the row's store key, so it
/// gets the same bound as [`key_name`].
pub(super) fn entity_id(eid: &[u8]) -> Result<(), i32> {
    if eid.len() > MAX_KEY_PART_LEN {
        return Err(EINVAL);
    }

    Ok(())
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

// An event name becomes its scope's schema, so every valid event name must be
// a valid segment.
const _: () = assert!(MAX_NAME_LEN <= MAX_SEGMENT_LEN);

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::{DbScope, GATEWAY_ASSETS_DB, NAMESPACE_GATEWAY, Sri, is_own_asset_scope};
    use super::{EINVAL, is_reserved_namespace, key_name, segment};
    use super::{MAX_KEY_PART_LEN, MAX_SEGMENT_LEN, entity_id};

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
    fn nul_in_a_key_name_is_rejected() {
        assert_eq!(key_name("a\0b"), Err(EINVAL));
        assert_eq!(key_name("\0"), Err(EINVAL));
        assert_eq!(key_name("a*b:c@d"), Ok(()));
        // An empty kv prefix lists everything; only segments must be non-empty.
        assert_eq!(key_name(""), Ok(()));
    }

    #[test]
    fn over_long_key_parts_are_rejected() {
        let name = |len: usize| "a".repeat(len);

        assert_eq!(
            segment(Cow::Owned(name(MAX_SEGMENT_LEN))),
            Ok(name(MAX_SEGMENT_LEN))
        );
        assert_eq!(key_name(&name(MAX_KEY_PART_LEN)), Ok(()));
        assert_eq!(key_name(&name(MAX_KEY_PART_LEN + 1)), Err(EINVAL));
        // The lsm-tree key cap itself.
        assert_eq!(key_name(&name(usize::from(u16::MAX))), Err(EINVAL));

        assert_eq!(entity_id(&[0; MAX_KEY_PART_LEN]), Ok(()));
        assert_eq!(entity_id(&[0; MAX_KEY_PART_LEN + 1]), Err(EINVAL));
    }

    #[test]
    fn segment_rejects_the_adversarial_name_matrix() {
        // Names a guest can make a panic, beyond the key-expression syntax.
        let long = |len: usize| "a".repeat(len);

        for name in [
            String::new(),
            String::from("\0"),
            String::from("a\0b"),
            // Would encode into the `sorg` keyspace were it accepted.
            String::from("sorg\0*node-lease\0*p\0:tbentries\0"),
            long(MAX_SEGMENT_LEN + 1),
            long(usize::from(u16::MAX) - 1),
            long(usize::from(u16::MAX)),
            long(usize::from(u16::MAX) + 1),
        ] {
            assert_eq!(
                segment(Cow::Owned(name.clone())),
                Err(EINVAL),
                "{} bytes: {:?}",
                name.len(),
                name.chars().take(8).collect::<String>()
            );
        }
    }

    #[test]
    fn key_expression_syntax_in_a_segment_is_rejected() {
        // The names that panicked the db plugin, and every other character
        // zenoh gives a meaning in a chunk.
        for name in ["x?", "x#", "*", "**", "$*", "a*b", "a/b", "/", "@x", "x@"] {
            assert_eq!(segment(Cow::Borrowed(name)), Err(EINVAL), "{name:?}");
        }

        // What legitimately passes through: public names, the gateway's
        // namespace and assets database, an SRI as asset schema, the defaults.
        let sri = Sri::of_path("chatty").expect("srn").to_string();
        for name in [
            "chatty",
            "CELLSISH",
            "my-app_v2.1",
            "gw",
            GATEWAY_ASSETS_DB,
            &sri,
            "d",
            "p",
        ] {
            assert_eq!(
                segment(Cow::Owned(name.to_owned())).as_deref(),
                Ok(name),
                "{name:?}"
            );
        }
    }
}
