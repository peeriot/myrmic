//! The child-side fencing verification pass (spec §3) for the hosted cell.
//! The decision core is `cell_protocol::supervision` — the exact logic the
//! Linux exec runs — driven here from evidence gathered over the db RPC.
//! One evidence rule throughout: only an affirmative, successful read counts;
//! a failed read (radio, timeout, db down) is "unknown" and changes nothing.

use alloc::borrow::ToOwned;
use alloc::string::ToString;
use alloc::vec::Vec;

use cell_protocol::supervision::{
    Evidence, FencingState, LeaseFacts, LeaseTracker, RowFacts, RowRead, Verdict, WatchedCell,
};
use cell_protocol::{
    CellAttachment, CellInstance, INSTANCE_REGISTRY_TABLE, MailboxCommand, NODE_LEASE_TABLE,
    NodeLease, PLACEMENT_TABLE, PlacementEntry, PlacementKind, RuntimeId, Sri,
    instance_registry_scope, node_lease_scope, placement_scope,
};
use db_client::v1::models as db_models;
use db_client::v1::{Client, models::Scope};
use embassy_time::{Instant, with_timeout};
use myrmic_common::cells::{CellLost, Command, LostReason, SYS_CELL_LOST};
use wasm_storage::__reexports::postcard;
use zenoh_nano::scout::ZenohIdProto;
use zenoh_result::zerror;

use crate::mailbox::{MailboxItem, publish_item};
use crate::service::DEFAULT_TIMEOUT;

/// One fencing pass over the hosted cell. Reads the cell's own placement
/// row, its parent's, and the parent node's lease — a single point read,
/// the only lease this single-cell host ever needs — and enforces a Kill
/// verdict: terminate the module and release the cell's rows. Owed
/// row cleanup is retried every pass, so a db outage cannot leave a stale
/// row blocking the SRI forever.
pub(crate) async fn verify_tick(
    client: &Client,
    zid: ZenohIdProto,
    cell_active: bool,
    watched: &mut Option<WatchedCell>,
    tracker: &mut LeaseTracker,
    fencing: &mut FencingState,
    owed: &mut Vec<Sri>,
) {
    drain_owed(client, owed).await;

    if !cell_active {
        return;
    }
    let Some(cell) = watched.as_ref() else {
        return;
    };

    let now_ms = Instant::now().as_millis();
    let my_exec: RuntimeId = zid.into();
    let self_row = read_row(client, &cell.sri, my_exec).await;
    let (parent_row, parent_lease_expired) = match (cell.lineage.parent, cell.lineage.detached) {
        (Some(parent), false) => {
            let row = read_row(client, &parent, my_exec).await;
            // The edge's grace overrides the parent node's declared ttl:
            // this cell's personal tolerance for parent silence.
            let expired = match row {
                RowRead::Ok((node, _)) => {
                    let lease = read_lease(client, node).await;
                    tracker.judge_read(node, lease, cell.lineage.grace_ms, now_ms)
                }
                _ => None,
            };
            (row, expired)
        }
        _ => (RowRead::Failed, None),
    };

    let evidence = Evidence {
        self_row,
        parent_row,
        parent_lease_expired,
    };
    if let Verdict::Kill(why) = fencing.evaluate(my_exec, cell, &evidence) {
        log::warn!("[fencing] killing cell '{sri}': {why:?}", sri = cell.sri);
        wasm_runtime::terminate_module();
        owed.push(cell.sri);
        *watched = None;
        drain_owed(client, owed).await;
    }
}

/// Sweeps placement rows naming this node from a previous boot. The node id
/// is stable (derived from the MAC) and cells never resume after a reboot,
/// so any placement naming this node that it is not hosting is a remnant
/// whose body died with the power: the parent is notified (crashed) and the
/// rows are released. Retried every tick until it completes cleanly;
/// returns whether it did. The hosted cell (if any) is skipped.
pub(crate) async fn boot_sweep(
    client: &Client,
    zid: ZenohIdProto,
    hosted: Option<&Sri>,
    owed: &mut Vec<Sri>,
) -> bool {
    let my_exec: RuntimeId = zid.into();
    let Ok(cells) = list_placements(client).await else {
        return false;
    };
    let mut done = true;
    for entry in cells {
        let placed_here =
            matches!(&entry.kind, PlacementKind::Wasm { runtime } if runtime.id() == my_exec);
        if !placed_here || hosted == Some(&entry.sri) {
            continue;
        }
        log::warn!(
            "[sweep] releasing '{sri}' from a previous boot",
            sri = entry.sri
        );
        match read_instance(client, &entry.sri).await {
            RowRead::Ok(instance) => {
                if !instance.lineage.detached
                    && let Some(parent) = instance.lineage.parent
                {
                    // Notification precedes cleanup: an unemitted note is
                    // retried next tick while the rows still exist.
                    if !emit_cell_lost(client, parent, &entry.sri, &instance).await {
                        done = false;
                        continue;
                    }
                }
            }
            RowRead::Absent => {}
            RowRead::Failed => {
                done = false;
                continue;
            }
        }
        owed.push(entry.sri);
    }
    done
}

/// Delivers `cell_lost { crashed }` into the parent's db mailbox, exactly as
/// the Linux hosts do (reserved system command, postcard payload).
async fn emit_cell_lost(client: &Client, parent: Sri, cell: &Sri, instance: &CellInstance) -> bool {
    let Ok(cmd) = Command::new(SYS_CELL_LOST.to_owned()) else {
        return false;
    };
    let note = CellLost {
        cell: *cell,
        local_name: instance.lineage.local_name.clone(),
        reason: LostReason::Crashed,
    };
    let Ok(payload) = postcard::to_allocvec(&note) else {
        return false;
    };
    let item = MailboxItem::Command {
        dest_sri: parent,
        command: MailboxCommand {
            cmd,
            payload: Some(payload),
            attachment: CellAttachment::default(),
        },
    };
    match with_timeout(DEFAULT_TIMEOUT, publish_item(client, item)).await {
        Ok(Ok(())) => true,
        _ => {
            log::warn!("[sweep] cell_lost to '{parent}' failed; retrying next tick");
            false
        }
    }
}

/// Lists every placement row; `Err` means no evidence.
async fn list_placements(client: &Client) -> Result<Vec<PlacementEntry>, ()> {
    let list = client.read_tx_in(placement_scope(), async move |client, id| {
        let response = client
            .send(db_models::tb_list::Request {
                id,
                op: db_models::tb_list::Op {
                    scope: placement_scope(),
                    table: PLACEMENT_TABLE.to_owned(),
                    cursor: None,
                    limit: None,
                    order: None,
                },
            })
            .await?
            .map_err(|err| zerror!("{}", err.message))?;
        Ok(response)
    });
    match with_timeout(DEFAULT_TIMEOUT, list).await {
        Ok(Ok(response)) => Ok(response
            .entities
            .into_iter()
            .filter_map(|(_, value)| postcard::from_bytes::<PlacementEntry>(&value).ok())
            .collect()),
        _ => Err(()),
    }
}

/// Reads a cell's instance row (its lineage) into evidence semantics.
async fn read_instance(client: &Client, sri: &Sri) -> RowRead<CellInstance> {
    let eid = sri.to_string().into_bytes();
    let read = client.read_tx_in(instance_registry_scope(), async move |client, id| {
        let response = client
            .send(db_models::tb_get::Request {
                id,
                op: db_models::tb_get::Op {
                    scope: instance_registry_scope(),
                    table: INSTANCE_REGISTRY_TABLE.to_owned(),
                    eid,
                },
            })
            .await?
            .map_err(|err| zerror!("{}", err.message))?;
        Ok(response)
    });
    match with_timeout(DEFAULT_TIMEOUT, read).await {
        Ok(Ok(response)) => match response.value {
            Some(value) => match postcard::from_bytes::<CellInstance>(&value) {
                Ok(instance) => RowRead::Ok(instance),
                Err(_) => RowRead::Failed,
            },
            None => RowRead::Absent,
        },
        _ => RowRead::Failed,
    }
}

/// Reads a placement row into fencing evidence.
async fn read_row(client: &Client, sri: &Sri, my_exec: RuntimeId) -> RowRead<RowFacts> {
    let eid = sri.to_string().into_bytes();
    let read = client.read_tx_in(placement_scope(), async move |client, id| {
        let response = client
            .send(db_models::tb_get::Request {
                id,
                op: db_models::tb_get::Op {
                    scope: placement_scope(),
                    table: PLACEMENT_TABLE.to_owned(),
                    eid,
                },
            })
            .await?
            .map_err(|err| zerror!("{}", err.message))?;
        Ok(response)
    });
    match with_timeout(DEFAULT_TIMEOUT, read).await {
        Ok(Ok(response)) => match response.value {
            Some(value) => match postcard::from_bytes::<PlacementEntry>(&value) {
                Ok(entry) => {
                    // Placeholder rows (mid-deploy) and bridge rows carry no
                    // exec placement; this exec's own id stands in and only
                    // the generation is compared.
                    let node = match &entry.kind {
                        PlacementKind::Wasm { runtime } => runtime.id(),
                        PlacementKind::Bridge { .. } | PlacementKind::Placeholder => my_exec,
                    };
                    RowRead::Ok((node, entry.gen_id))
                }
                Err(_) => RowRead::Failed,
            },
            None => RowRead::Absent,
        },
        _ => RowRead::Failed,
    }
}

/// Point-reads one node's lease row into evidence semantics (its seq and
/// declared ttl).
async fn read_lease(client: &Client, node: RuntimeId) -> RowRead<LeaseFacts> {
    let eid = node.to_string().into_bytes();
    let read = client.read_tx_in(node_lease_scope(), async move |client, id| {
        let response = client
            .send(db_models::tb_get::Request {
                id,
                op: db_models::tb_get::Op {
                    scope: node_lease_scope(),
                    table: NODE_LEASE_TABLE.to_owned(),
                    eid,
                },
            })
            .await?
            .map_err(|err| zerror!("{}", err.message))?;
        Ok(response)
    });
    match with_timeout(DEFAULT_TIMEOUT, read).await {
        Ok(Ok(response)) => match response.value {
            Some(value) => match postcard::from_bytes::<NodeLease>(&value) {
                Ok(lease) => RowRead::Ok((lease.seq, lease.ttl_ms)),
                Err(_) => RowRead::Failed,
            },
            None => RowRead::Absent,
        },
        _ => RowRead::Failed,
    }
}

/// Attempts every owed row cleanup, keeping what still fails.
async fn drain_owed(client: &Client, owed: &mut Vec<Sri>) {
    if owed.is_empty() {
        return;
    }
    let pending = core::mem::take(owed);
    for sri in pending {
        if release_rows(client, &sri).await {
            log::info!("[fencing] released rows for '{sri}'");
        } else {
            log::debug!("[fencing] row cleanup for '{sri}' pending");
            owed.push(sri);
        }
    }
}

/// Placement row first, instance row second (the Linux ordering); both
/// deletes are idempotent, so a retry after partial success is safe.
async fn release_rows(client: &Client, sri: &Sri) -> bool {
    delete_row(client, placement_scope(), PLACEMENT_TABLE, sri).await
        && delete_row(
            client,
            instance_registry_scope(),
            INSTANCE_REGISTRY_TABLE,
            sri,
        )
        .await
}

/// `true` when the db answered at all: an op-level error (e.g. the row is
/// already gone) counts as done; only transport failures are retried.
async fn delete_row(client: &Client, scope: Scope, table: &str, sri: &Sri) -> bool {
    let eid = sri.to_string().into_bytes();
    let table = table.to_owned();
    let delete = client.write_tx_in(scope.clone(), async move |client, id| {
        // The op-level result is returned untouched: a "row missing" error
        // still means the db answered, which is all the caller needs.
        let result = client
            .send(db_models::tb_delete::Request {
                id,
                op: db_models::tb_delete::Op { scope, table, eid },
            })
            .await?;
        Ok(result)
    });
    matches!(with_timeout(DEFAULT_TIMEOUT, delete).await, Ok(Ok(_)))
}
