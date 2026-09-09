//! A cell implemented by this node's own firmware.
//!
//! The node hosts one cell. Normally the orchestrator fills that slot by
//! deploying a WASM module; a firmware that declares a [`NativeCell`] fills it
//! itself, and no module is ever loaded here.
//!
//! Nothing about the identity needs the network — the SRI is folded from the
//! SRN offline by the firmware — so the slot is occupied from boot. What this
//! module does is make the swarm agree: it writes the placement row that lets
//! other cells address this one, and that marks the node occupied so the
//! orchestrator will not also place a WASM cell here.
//!
//! The row is (re)asserted on the same reconcile tick as this node's exec
//! registration and lease renewal, so a reconnect or a db outage heals rather
//! than silently un-registering the cell.

use alloc::borrow::ToOwned;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use cell_protocol::{
    CellInstance, Gen, INSTANCE_REGISTRY_TABLE, PLACEMENT_TABLE, PlacementEntry, PlacementKind,
    SpawnLineage, Sri, instance_registry_scope, placement_scope,
};
use db_client::v1::models::tb_insert;
use db_client::v1::{Client, models::Scope};
use embassy_time::with_timeout;
use myrmic_common::cells::Command;
use wasm_storage::__reexports::postcard;
use zenoh_nano::scout::ZenohIdProto;
use zenoh_result::zerror;

use crate::service::DEFAULT_TIMEOUT;

/// The class name a native cell is reported under. It has no WASM class — the
/// firmware *is* the implementation — but tooling expects a name to show.
const NATIVE_CLASS: &str = "<firmware>";

/// A cell this node's firmware implements.
#[derive(Debug, Clone)]
pub struct NativeCell {
    /// The cell's identity, folded from its SRN by the firmware.
    pub sri: Sri,
    /// The commands it answers, as advertised to the swarm.
    pub commands: Vec<Command>,
    /// The SRN it was declared under, kept for display.
    pub name: String,
    /// Called once the placement row is committed and the cell is reachable.
    pub online: fn(),
}

/// Writes the rows that make `native` addressable: its placement (which also
/// marks this node occupied) and its instance row (so tooling can name it).
///
/// Returns whether the swarm now agrees. A `false` is not fatal — the caller
/// retries on the next reconcile tick, which is also what re-asserts the claim
/// after a reconnect.
pub(crate) async fn claim(
    client: &Client,
    zid: ZenohIdProto,
    native: &NativeCell,
    gen_id: Gen,
) -> bool {
    if !write_row(
        client,
        placement_scope(),
        PLACEMENT_TABLE,
        native.sri,
        &PlacementEntry {
            sri: native.sri,
            kind: PlacementKind::Native {
                runtime: zid.into(),
            },
            app: None,
            gen_id,
        },
    )
    .await
    {
        return false;
    }

    // Best effort: without it the cell is still addressable, just anonymous in
    // `m cells`. Not worth failing the claim over.
    let _ = write_row(
        client,
        instance_registry_scope(),
        INSTANCE_REGISTRY_TABLE,
        native.sri,
        &CellInstance {
            sri: native.sri,
            class_name: NATIVE_CLASS.to_owned(),
            gen_id,
            lineage: SpawnLineage {
                // A native cell is a root: nothing spawned it, so nothing
                // fences it and no parent is owed a `cell_lost`.
                parent: None,
                parent_gen_id: None,
                detached: false,
                local_name: Some(native.name.clone()),
                grace_ms: None,
                deadline_ms: None,
            },
        },
    )
    .await;

    true
}

async fn write_row<T: serde::Serialize>(
    client: &Client,
    scope: Scope,
    table: &str,
    sri: Sri,
    value: &T,
) -> bool {
    let Ok(bytes) = postcard::to_allocvec(value) else {
        log::error!("[native] failed to serialize the {table} row");
        return false;
    };
    let eid = sri.to_string().into_bytes();
    let table = table.to_owned();
    let write = client.write_tx_in(scope.clone(), async move |client, tx_id| {
        client
            .send(tb_insert::Request {
                id: tx_id,
                op: tb_insert::Op {
                    scope,
                    table,
                    eid: Some(eid),
                    value: bytes,
                },
            })
            .await
            .map_err(|e| zerror!("unable to send insert: {e}"))?
            .map_err(|e| zerror!("insert rejected: {}", e.message))?;
        Ok(())
    });

    matches!(with_timeout(DEFAULT_TIMEOUT, write).await, Ok(Ok(())))
}

/// A generation for this incarnation of the native cell.
///
/// Each boot is a new life of the same SRI, so the stamp must advance across
/// one. Wall time supplies that; the node id makes it unique between devices.
/// Before the first clock sync there is no wall time, and the caller waits
/// rather than minting an ancient-looking generation.
pub(crate) fn incarnation(zid: ZenohIdProto, wall_time_ms: u64) -> Gen {
    let mut id = [0u8; 16];
    let zid_bytes = zid.to_le_bytes();
    let n = zid_bytes.len().min(16);
    id[..n].copy_from_slice(&zid_bytes[..n]);
    Gen::from_parts(wall_time_ms, u128::from_le_bytes(id))
}
