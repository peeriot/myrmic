//! The child-side fencing verification pass (spec §3) for the hosted cell.
//! The decision core is `cell_protocol::supervision` — the exact logic the
//! Linux exec runs — driven here from evidence gathered over the db RPC.
//! One evidence rule throughout: only an affirmative, successful read counts;
//! a failed read (radio, timeout, db down) is "unknown" and changes nothing.

use alloc::borrow::ToOwned;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use cell_protocol::generations::{
    GENERATIONS, generation_key, generations_of, newest_instances, newest_placements,
};
use cell_protocol::supervision::{
    Evidence, FencingState, LeaseFacts, LeaseTracker, RowFacts, RowRead, Verdict, WatchedCell,
};
use cell_protocol::{
    CellAttachment, CellInstance, Gen, MailboxCommand, NODE_LEASE_TABLE, NodeLease, PlacementEntry,
    PlacementKind, RuntimeId, Sri, instance_registry_scope, node_lease_scope, placement_scope,
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
    owed: &mut Vec<(Sri, Gen)>,
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
        owed.push((cell.sri, cell.gen_id));
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
    owed: &mut Vec<(Sri, Gen)>,
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
        owed.push((entry.sri, entry.gen_id));
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

/// Lists every cell's newest placement; `Err` means no evidence.
async fn list_placements(client: &Client) -> Result<Vec<PlacementEntry>, ()> {
    let entries = list_rows(client, placement_scope(), GENERATIONS.to_owned()).await?;

    Ok(newest_placements(entries)
        .into_iter()
        .filter_map(Result::ok)
        .collect())
}

/// Reads a cell's newest instance row (its lineage) into evidence semantics.
async fn read_instance(client: &Client, sri: &Sri) -> RowRead<CellInstance> {
    let Ok(entries) = list_rows(client, instance_registry_scope(), generations_of(sri)).await
    else {
        return RowRead::Failed;
    };

    // The prefix covers this one SRI, so at most one instance comes back.
    match newest_instances(entries).pop() {
        Some(Ok(instance)) => RowRead::Ok(instance),
        Some(Err(_)) => RowRead::Failed,
        None => RowRead::Absent,
    }
}

/// Reads a cell's newest placement row into fencing evidence.
async fn read_row(client: &Client, sri: &Sri, my_exec: RuntimeId) -> RowRead<RowFacts> {
    let Ok(entries) = list_rows(client, placement_scope(), generations_of(sri)).await else {
        return RowRead::Failed;
    };

    // The prefix covers this one SRI, so at most one placement comes back.
    match newest_placements(entries).pop() {
        Some(Ok(entry)) => {
            // Placeholder rows (mid-deploy) and bridge rows carry no
            // exec placement; this exec's own id stands in and only
            // the generation is compared.
            let node = entry.kind.host().unwrap_or(my_exec);
            RowRead::Ok((node, entry.gen_id))
        }
        Some(Err(_)) => RowRead::Failed,
        None => RowRead::Absent,
    }
}

/// The rows (keys with values) under `prefix` in `scope`; `Err` means no
/// evidence.
async fn list_rows(
    client: &Client,
    scope: Scope,
    prefix: String,
) -> Result<Vec<(String, Vec<u8>)>, ()> {
    let list = client.read_tx_in(scope.clone(), async move |client, id| {
        let response = client
            .send(db_models::key_list::Request {
                id,
                op: db_models::key_list::Op { scope, prefix },
            })
            .await?
            .map_err(|err| zerror!("{}", err.message))?;
        Ok(response)
    });
    match with_timeout(DEFAULT_TIMEOUT, list).await {
        Ok(Ok(response)) => Ok(response.entries),
        _ => Err(()),
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
async fn drain_owed(client: &Client, owed: &mut Vec<(Sri, Gen)>) {
    if owed.is_empty() {
        return;
    }
    let pending = core::mem::take(owed);
    for (sri, gen_id) in pending {
        if release_rows(client, &sri, gen_id).await {
            log::info!("[fencing] released rows for '{sri}'");
        } else {
            log::debug!("[fencing] row cleanup for '{sri}' pending");
            owed.push((sri, gen_id));
        }
    }
}

/// Placement row first, instance row second (the Linux ordering), both of
/// generation `gen_id` only: a newer life of the cell elsewhere keeps its rows.
/// Both deletes are idempotent, so a retry after partial success is safe.
async fn release_rows(client: &Client, sri: &Sri, gen_id: Gen) -> bool {
    let key = generation_key(sri, gen_id);
    delete_row(client, placement_scope(), key.clone()).await
        && delete_row(client, instance_registry_scope(), key).await
}

/// Adds a tombstone for `key` in `scope`. `true` when the db answered at all:
/// an op-level error counts as done; only transport failures are retried.
async fn delete_row(client: &Client, scope: Scope, key: String) -> bool {
    let delete = client.write_tx_in(scope.clone(), async move |client, id| {
        // The op-level result is returned untouched: an op-level error still
        // means the db answered, which is all the caller needs.
        let result = client
            .send(db_models::key_delete::Request {
                id,
                op: db_models::key_delete::Op { scope, key },
            })
            .await?;
        Ok(result)
    });
    matches!(with_timeout(DEFAULT_TIMEOUT, delete).await, Ok(Ok(_)))
}
