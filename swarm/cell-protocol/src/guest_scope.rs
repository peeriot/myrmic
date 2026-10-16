//! The db scopes a cell may name, and what they resolve to.
//!
//! Shared by every host path that takes a scope from a cell — its db calls,
//! and the gateway serving an asset reply — so the same scope is reachable, or
//! refused, either way.

use crate::sys::borrow::Cow;
use crate::sys::string::{String, ToString};
use crate::{GATEWAY_ASSETS_DB, NAMESPACE_CELLS, NAMESPACE_GATEWAY, Sri, scope_of_cell};
use db_commons::models::Scope;
use myrmic_common::db::{Namespace, Scope as GuestScope};
use myrmic_common::types::error::{EINVAL, EPERM};

/// Resolves a scope cell `sri` named into the db scope it addresses.
///
/// `Namespace::Private` is the cell's own database. A public scope may not
/// name a reserved namespace, nor anything in the gateway namespace beyond the
/// cell's own assets. Fails with `EPERM` for a refused scope and `EINVAL` for a
/// malformed segment.
pub fn resolve_guest_scope(sri: &Sri, scope: GuestScope) -> Result<Scope, i32> {
    let (ns, db, schema) = scope.into_inner();

    let mut scope = match ns {
        // The cell's own database under the cells namespace, stamped by the
        // host — the guest never names it (and can't pick another database).
        Namespace::Private => scope_of_cell(sri),
        Namespace::Public(public_ns) => {
            if is_reserved_namespace(public_ns.as_ref()) {
                return Err(EPERM);
            }

            let mut scope = Scope {
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

    if scope.namespace == NAMESPACE_GATEWAY && !is_own_asset_scope(&scope, sri) {
        return Err(EPERM);
    }

    Ok(scope)
}

/// A segment the guest named explicitly must be a valid [`key_name`] and
/// non-empty — `Some("")` is a guest bug, not a request for the default.
fn segment(segment: Cow<'static, str>) -> Result<String, i32> {
    key_name(&segment)?;
    if segment.is_empty() {
        return Err(EINVAL);
    }

    Ok(segment.into_owned())
}

/// Any name the guest hands the db to build a key from — scope segments, kv
/// keys and prefixes, tables, measurements, paths — must not contain NUL.
/// Keys end each name at a NUL, so one inside would let the name reach into
/// another scope's keys. The db refuses such a name too; this makes it an
/// `EINVAL` rather than a failed call.
pub fn key_name(name: &str) -> Result<(), i32> {
    if name.contains('\0') {
        return Err(EINVAL);
    }

    Ok(())
}

/// Whether `scope` is the one place in the gateway namespace `sri` owns: its
/// own assets.
///
/// The namespace can't simply be reserved — a cell publishes its own static
/// assets there. Everything else in it belongs to the gateway: the routing
/// table, and every other cell's assets. Checked against the identity the host
/// stamped, never one the guest supplied.
fn is_own_asset_scope(scope: &Scope, sri: &Sri) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn gateway_scope(database: &str, schema: &str) -> Scope {
        Scope {
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
    fn private_resolves_to_the_cells_own_database() {
        let me = Sri::of_path("chatty").expect("srn");
        let scope = resolve_guest_scope(&me, GuestScope::private()).expect("scope");

        assert_eq!(scope, scope_of_cell(me));
    }

    #[test]
    fn reserved_and_foreign_gateway_scopes_are_refused() {
        let me = Sri::of_path("chatty").expect("srn");
        let other = Sri::of_path("other").expect("srn");

        assert_eq!(
            resolve_guest_scope(&me, GuestScope::public("sorg")),
            Err(EPERM)
        );
        let foreign = myrmic_common::gateway::asset_scope(&other.to_string());
        assert_eq!(resolve_guest_scope(&me, foreign), Err(EPERM));
        let own = myrmic_common::gateway::asset_scope(&me.to_string());
        assert!(resolve_guest_scope(&me, own).is_ok());
    }

    #[test]
    fn empty_segments_are_rejected() {
        assert_eq!(segment(Cow::Borrowed("")), Err(EINVAL));
        assert_eq!(segment(Cow::Borrowed("d")).as_deref(), Ok("d"));
    }

    #[test]
    fn nul_in_a_segment_is_rejected() {
        // Would encode into the `sorg` keyspace were it accepted.
        assert_eq!(
            segment(Cow::Borrowed("sorg\0*node-lease\0*p\0:tbentries\0")),
            Err(EINVAL)
        );
        assert_eq!(segment(Cow::Borrowed("a\0*b")), Err(EINVAL));
        assert_eq!(segment(Cow::Borrowed("\0")), Err(EINVAL));
    }

    #[test]
    fn nul_in_a_key_name_is_rejected() {
        assert_eq!(key_name("a\0b"), Err(EINVAL));
        assert_eq!(key_name("\0"), Err(EINVAL));
        assert_eq!(key_name("a*b:c@d"), Ok(()));
        // An empty kv prefix lists everything; only segments must be non-empty.
        assert_eq!(key_name(""), Ok(()));
    }
}
