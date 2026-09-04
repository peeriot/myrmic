use std::time::Duration;

use crate::wasm::{
    cell::state::CellState,
    host_functions::{as_slice, as_slice_mut},
};
use cell_protocol::{BlobHash, Gen, Sri};
use myrmic_common::cells::{
    ClassRef, SPAWN_ERR_ALREADY_EXISTS, SPAWN_ERR_CLASS_NOT_FOUND, SPAWN_ERR_DEPLOY_FAILED,
    SpawnRequest, TERMINATE_ERR_ERASE_FAILED, TERMINATE_ERR_NOT_FOUND, TERMINATE_ERR_NOT_PERMITTED,
    TERMINATE_ERR_UNDEPLOY_FAILED,
};
use myrmic_common::types::error::SUCCESS;
use sorg_common::spawn_gate::{
    ExistingInstance, ExistingPlacement, GateDecision, LeaseView, evaluate_spawn_gate,
    is_self_or_descendant,
};
use sorg_common::{
    RequirementTags, SpawnLineage, class_registry, deploy_wasm_cell, instance_registry,
    undeploy_cell,
};
use tracing::error;
use uuid::Uuid;
use wasmtime::Caller;

const DEPLOY_TIMEOUT: Duration = Duration::from_secs(20);

async fn resolve_class_name(session: &zenoh::Session, class: ClassRef) -> Result<String, i32> {
    match class {
        ClassRef::Name(name) => match class_registry::get_class_info(session, &name).await {
            Ok(Some(_)) => Ok(name),
            Ok(None) => {
                error!("spawn: class '{name}' not found");
                Err(SPAWN_ERR_CLASS_NOT_FOUND)
            }
            Err(err) => {
                error!("spawn: failed to look up class '{name}': {err}");
                Err(SPAWN_ERR_CLASS_NOT_FOUND)
            }
        },
        ClassRef::Hash(hash) => {
            let target = BlobHash::Sha2(hash);
            match class_registry::list_classes(session).await {
                Ok(classes) => match classes.into_iter().find(|c| c.wasm_hash == Some(target)) {
                    Some(info) => Ok(info.name),
                    None => {
                        error!("spawn: no class found for hash {hash:?}");
                        Err(SPAWN_ERR_CLASS_NOT_FOUND)
                    }
                },
                Err(err) => {
                    error!("spawn: failed to list classes: {err}");
                    Err(SPAWN_ERR_CLASS_NOT_FOUND)
                }
            }
        }
    }
}

pub(crate) async fn spawn_cell(
    mut caller: Caller<'_, CellState>,
    buffer_ptr: u32,
    length: u32,
    out_sri_ptr: u32,
) -> i32 {
    let request = {
        let bytes = as_slice(&mut caller, buffer_ptr as usize, length as usize);
        match postcard::from_bytes::<SpawnRequest>(bytes) {
            Ok(r) => r,
            Err(err) => {
                error!("spawn: failed to decode spawn request: {err}");
                return SPAWN_ERR_DEPLOY_FAILED;
            }
        }
    };

    let session = caller.data().session().clone();
    // The spawn tree is the name tree: the child's SRI is derived from the
    // *calling* cell's identity (verified host-side) and the requested local
    // name. The caller cannot forge a parent it does not own.
    let parent = *caller.data().sri();
    let parent_gen_id = caller.data().gen_id();

    let class_name = match resolve_class_name(&session, request.class).await {
        Ok(name) => name,
        Err(code) => return code,
    };

    let local_name = request
        .local_name
        .unwrap_or_else(|| Uuid::new_v4().simple().to_string());

    let child = match parent.child(&local_name) {
        Ok(sri) => sri,
        Err(err) => {
            error!("spawn: invalid local name '{}': {err}", local_name);
            return SPAWN_ERR_DEPLOY_FAILED;
        }
    };

    if let Err(code) = gate_existing_instance(
        &session,
        &parent,
        child,
        &local_name,
        request.detached,
        parent_gen_id,
    )
    .await
    {
        return code;
    }

    let tags = request.tags.map(RequirementTags::new).unwrap_or_default();

    let lineage = SpawnLineage {
        parent: Some(parent),
        parent_gen_id: Some(parent_gen_id),
        detached: request.detached,
        local_name: Some(local_name.clone()),
        grace_ms: request.grace_ms,
        deadline_ms: request.deadline_ms,
    };
    let result = deploy_wasm_cell(
        &session,
        child,
        &class_name,
        tags,
        DEPLOY_TIMEOUT,
        lineage,
        request.arguments,
        // A spawned cell inherits its parent's app at deploy time.
        None,
    )
    .await;

    if let Err(err) = result {
        error!("spawn: failed to deploy '{child}': {err}");
        notify_spawn_failed(&session, &parent, child, &local_name, request.detached).await;
        return SPAWN_ERR_DEPLOY_FAILED;
    }

    // Hand the derived child SRI back to the caller (16 raw UUID bytes).
    let out = as_slice_mut(&mut caller, out_sri_ptr as usize, 16);
    out.copy_from_slice(&child.to_bytes());

    SUCCESS
}

/// Gate the spawn against any existing instance row for the child SRI,
/// releasing the stale rows first when the gate resolves Supersede.
async fn gate_existing_instance(
    session: &zenoh::Session,
    parent: &Sri,
    child: Sri,
    local_name: &str,
    detached: bool,
    parent_gen_id: Gen,
) -> Result<(), i32> {
    let existing = match instance_registry::get_instance(session, &child).await {
        Ok(existing) => existing,
        Err(err) => {
            error!("spawn: failed to check instance '{child}': {err}");
            notify_spawn_failed(session, parent, child, local_name, detached).await;
            return Err(SPAWN_ERR_DEPLOY_FAILED);
        }
    };
    let decision = match &existing {
        None => GateDecision::Admit,
        Some(info) => {
            // Placement is presence-based from this vantage: the host fn has
            // no lease-staleness observer, so an expired-lease corpse resolves
            // AlreadyExists here until hygiene releases its rows (the SDK
            // retries respawns). Stale-parent-edge supersede is exact.
            let placement = match sorg_common::get_placement(session, &child).await {
                Ok(Some(_)) => Some(ExistingPlacement {
                    lease: LeaseView::Live,
                }),
                Ok(None) => None,
                Err(err) => {
                    error!("spawn: failed to check placement '{child}': {err}");
                    notify_spawn_failed(session, parent, child, local_name, detached).await;
                    return Err(SPAWN_ERR_DEPLOY_FAILED);
                }
            };
            evaluate_spawn_gate(
                Some(&ExistingInstance {
                    detached: info.lineage.detached,
                    parent_gen_id: info.lineage.parent_gen_id,
                }),
                placement.as_ref(),
                Some(parent_gen_id),
            )
        }
    };
    match decision {
        GateDecision::Admit => Ok(()),
        GateDecision::AlreadyExists => {
            error!("spawn: instance '{child}' already exists");
            Err(SPAWN_ERR_ALREADY_EXISTS)
        }
        GateDecision::Supersede => {
            tracing::info!(
                child = %child,
                old_parent_instance = ?existing.as_ref().and_then(|i| i.lineage.parent_gen_id),
                new_parent_instance = %parent_gen_id,
                "spawn: superseding stale instance"
            );
            if let Err(err) = sorg_common::remove_placement(session, &child).await {
                error!("spawn: failed to release stale placement '{child}': {err}");
                notify_spawn_failed(session, parent, child, local_name, detached).await;
                return Err(SPAWN_ERR_DEPLOY_FAILED);
            }
            // Aborting on erase failure keeps the corpse row from silently
            // suppressing the fresh cell's #[init].
            if let Err(err) = instance_registry::erase_instance(session, &child).await {
                error!("spawn: failed to erase stale instance '{child}': {err}");
                notify_spawn_failed(session, parent, child, local_name, detached).await;
                return Err(SPAWN_ERR_DEPLOY_FAILED);
            }
            Ok(())
        }
    }
}

/// A spawn that failed after its name was claimed fires the caller's monitor
/// too (never for detached spawns): recovery lives in the supervision loop,
/// not at every call site, so the loop must hear about children that never
/// came up — the spawn call's error code alone would strand it.
async fn notify_spawn_failed(
    session: &zenoh::Session,
    parent: &Sri,
    child: Sri,
    local_name: &str,
    detached: bool,
) {
    if detached {
        return;
    }
    let note = myrmic_common::cells::CellLost {
        cell: child,
        local_name: Some(local_name.to_owned()),
        reason: myrmic_common::cells::LostReason::SpawnFailed,
    };
    if let Err(err) = sorg_common::emit_cell_lost(session, parent, note).await {
        error!("spawn: cell_lost (spawn_failed) to '{parent}' failed: {err}");
    }
}

pub(crate) async fn terminate_cell(
    mut caller: Caller<'_, CellState>,
    buffer_ptr: u32,
    length: u32,
) -> i32 {
    let sri = {
        let bytes = as_slice(&mut caller, buffer_ptr as usize, length as usize);
        let target = match std::str::from_utf8(bytes) {
            Ok(s) => s,
            Err(err) => {
                error!("terminate: invalid UTF-8 in SRI: {err}");
                return TERMINATE_ERR_NOT_FOUND;
            }
        };
        match Sri::from_target(target) {
            Ok(sri) => sri,
            Err(err) => {
                error!("terminate: invalid SRI/SRN '{target}': {err}");
                return TERMINATE_ERR_NOT_FOUND;
            }
        }
    };

    let session = caller.data().session().clone();
    let caller_sri = *caller.data().sri();

    let instances = match instance_registry::list_instances(&session).await {
        Ok(list) => list,
        Err(err) => {
            error!("terminate: failed to read instance registry: {err}");
            return TERMINATE_ERR_NOT_FOUND;
        }
    };
    let by_sri: std::collections::HashMap<_, _> =
        instances.iter().cloned().map(|i| (i.sri, i)).collect();
    let Some(target) = by_sri.get(&sri).cloned() else {
        error!("terminate: cell '{sri}' not found");
        return TERMINATE_ERR_NOT_FOUND;
    };
    // Kill authority is ancestry: the target must be the caller itself or
    // lie below it in the spawn tree.
    if !is_self_or_descendant(&by_sri, &caller_sri, &sri) {
        error!("terminate: '{sri}' is not a descendant of '{caller_sri}'");
        return TERMINATE_ERR_NOT_PERMITTED;
    }
    let descendants = sorg_common::spawn_gate::collect_subtree(&instances, &sri);

    if let Err(err) = undeploy_cell(&session, sri, DEPLOY_TIMEOUT).await {
        error!("terminate: failed to undeploy '{sri}': {err}");
        return TERMINATE_ERR_UNDEPLOY_FAILED;
    }

    if let Err(err) = instance_registry::erase_instance(&session, &sri).await {
        error!("terminate: failed to erase instance '{sri}': {err}");
        return TERMINATE_ERR_ERASE_FAILED;
    }

    // Cascade below the target; deaths inside a dying subtree cross no
    // supervision boundary, so only the target's own parent is notified.
    if !descendants.is_empty() {
        tracing::info!(target = %sri, subtree = descendants.len(), "terminate: cascading");
        let reap_session = session.clone();
        tokio::spawn(async move { reap_cells(&reap_session, descendants).await });
    }
    if let Err(err) = sorg_common::report_cell_death(
        &session,
        sri,
        target.gen_id,
        target.lineage.parent,
        target.lineage.detached,
        target.lineage.local_name.clone(),
        myrmic_common::cells::LostReason::Terminated,
    )
    .await
    {
        error!("terminate: death report for '{sri}' failed: {err}");
    }

    SUCCESS
}

/// Reaps a set of cells: orchestrator-mediated undeploy, then instance-row
/// erase, each tolerant of already-gone state. Descendants the walk cannot
/// reach die via fencing (spec §3).
async fn reap_cells(session: &zenoh::Session, sris: Vec<Sri>) {
    for sri in sris {
        if let Err(err) = undeploy_cell(session, sri, DEPLOY_TIMEOUT).await {
            tracing::debug!("reap: undeploy '{sri}' pending fencing: {err}");
        }
        if let Err(err) = instance_registry::erase_instance(session, &sri).await {
            tracing::debug!("reap: erase '{sri}': {err}");
        }
    }
}

/// `stop_self(code)`: kills the calling cell and its whole subtree (spec §6).
/// The parent is notified with the cell's stated reason; descendants die
/// silently (their deaths cross no supervision boundary). The kills run on a
/// host-side task that survives the calling cell, self last — the guest is
/// single-threaded, so it cannot spawn replacements while this returns.
pub(crate) async fn stop_self(caller: Caller<'_, CellState>, code_present: u32, code: u32) -> i32 {
    let session = caller.data().session().clone();
    let self_sri = *caller.data().sri();
    let self_gen = caller.data().gen_id();
    let lineage = caller.data().lineage().clone();
    let stop_code = (code_present != 0).then_some(code);

    let descendants = match instance_registry::list_instances(&session).await {
        Ok(instances) => sorg_common::spawn_gate::collect_subtree(&instances, &self_sri),
        Err(err) => {
            error!("stop_self: failed to read instance registry: {err}");
            Vec::new()
        }
    };
    tracing::info!(
        sri = %self_sri,
        code = ?stop_code,
        subtree = descendants.len(),
        "stop_self: cell stopping"
    );

    if let Err(err) = sorg_common::report_cell_death(
        &session,
        self_sri,
        self_gen,
        lineage.parent,
        lineage.detached,
        lineage.local_name.clone(),
        myrmic_common::cells::LostReason::Stopped { code: stop_code },
    )
    .await
    {
        error!("stop_self: death report for '{self_sri}' failed: {err}");
    }

    // Self goes last so this host call has issued everything before the
    // poison lands; the reaper task outlives the calling cell.
    tokio::spawn(async move {
        reap_cells(&session, descendants).await;
        reap_cells(&session, vec![self_sri]).await;
    });

    SUCCESS
}
