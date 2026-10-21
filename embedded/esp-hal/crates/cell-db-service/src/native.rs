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

use alloc::string::String;
use alloc::vec::Vec;

use cell_protocol::generations::{
    InstanceRow, PlacementRow, generation_key, generations_of, parse_generation_key,
};
use cell_protocol::{
    Gen, NATIVE_FIRMWARE_CLASS, PlacementKind, SpawnLineage, Sri, instance_registry_scope,
    placement_scope,
};
use db_client::v1::models::{key_delete, key_prefix, key_put};
use db_client::v1::{Client, models::Scope};
use embassy_time::with_timeout;
use myrmic_common::cells::Command;
use wasm_storage::__reexports::postcard;
use zenoh_nano::scout::ZenohIdProto;
use zenoh_result::zerror;

use crate::service::DEFAULT_TIMEOUT;

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
    if !write_generation(
        client,
        placement_scope(),
        native.sri,
        gen_id,
        &PlacementRow {
            kind: PlacementKind::Native {
                runtime: zid.into(),
            },
            app: None,
        },
    )
    .await
    {
        return false;
    }

    // Best effort: without it the cell is still addressable, just anonymous in
    // `m cells`. Not worth failing the claim over; the next tick writes it again.
    let instance_written = write_generation(
        client,
        instance_registry_scope(),
        native.sri,
        gen_id,
        &InstanceRow {
            class: NATIVE_FIRMWARE_CLASS,
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
    if !instance_written {
        log::warn!("[native] instance row of '{}' not written", native.sri);
    }

    true
}

/// Writes generation `gen_id` of `sri`'s row in `scope` and tombstones the
/// older ones in the same transaction: each boot is a new generation, and the
/// previous boot's row must not stay current next to it.
async fn write_generation<T: serde::Serialize>(
    client: &Client,
    scope: Scope,
    sri: Sri,
    gen_id: Gen,
    value: &T,
) -> bool {
    let Ok(bytes) = postcard::to_allocvec(value) else {
        log::error!(
            "[native] failed to serialize the row for {}",
            scope.database
        );
        return false;
    };
    let write = client.write_tx_in(scope.clone(), async move |client, tx_id| {
        let keys = client
            .send(key_prefix::Request {
                id: tx_id,
                op: key_prefix::Op {
                    scope: scope.clone(),
                    prefix: generations_of(&sri),
                },
            })
            .await
            .map_err(|e| zerror!("unable to send prefix read: {e}"))?
            .map_err(|e| zerror!("prefix read rejected: {}", e.message))?
            .keys;
        let older = keys
            .into_iter()
            .filter(|key| parse_generation_key(key).is_some_and(|(_, existing)| existing < gen_id));
        for key in older {
            client
                .send(key_delete::Request {
                    id: tx_id,
                    op: key_delete::Op {
                        scope: scope.clone(),
                        key,
                    },
                })
                .await
                .map_err(|e| zerror!("unable to send delete: {e}"))?
                .map_err(|e| zerror!("delete rejected: {}", e.message))?;
        }
        client
            .send(key_put::Request {
                id: tx_id,
                op: key_put::Op {
                    scope,
                    key: generation_key(&sri, gen_id),
                    value: bytes,
                },
            })
            .await
            .map_err(|e| zerror!("unable to send put: {e}"))?
            .map_err(|e| zerror!("put rejected: {}", e.message))?;
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
