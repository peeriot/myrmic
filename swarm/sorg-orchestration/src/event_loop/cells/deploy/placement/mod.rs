mod optimizer;
mod preprocessing;
mod triage;

use std::collections::{HashMap, HashSet};
use std::time::SystemTime;

use cell_protocol::{ClassInfo, PlacementKind, RuntimeId, Sri, placement_scope};
use sorg_common::{
    CellConfig, CellDeployment, DbClient, DeploymentError, ExecRuntimeInfo, TxId, class_registry,
    exec_registry, list_placements_in_tx, node_lease, supervision::SupervisionTiming, tx_begin,
    tx_commit,
};
use tracing::debug;

use crate::Result;

const MAX_COMMIT_RETRIES: u32 = 3;

/// The information relevant for the placement of a batch of cells. Focuses on the deployment
/// intent (how do we want to deploy these specific cells)
pub(crate) struct PlacementRequest {
    cells: Vec<CellDeployment>,
}

impl PlacementRequest {
    pub(crate) fn for_cells(cells: Vec<CellDeployment>) -> Self {
        Self { cells }
    }

    pub(crate) fn cells(&self) -> &[CellDeployment] {
        &self.cells
    }
}

/// The system information relevant for the placement of cells. Focuses on the current state of the
/// system, prior to the deployment we are currently doing
pub(crate) struct PlacementContext {
    execs: Vec<ExecRuntimeInfo>,
    class_info: HashMap<String, ClassInfo>,
    /// For each runtime, the SRIs of the cells currently hosted on it.
    cells_per_runtime: HashMap<RuntimeId, Vec<Sri>>,
}

impl PlacementContext {
    pub(crate) fn execs(&self) -> &[ExecRuntimeInfo] {
        &self.execs
    }

    pub(crate) fn class_info(&self) -> &HashMap<String, ClassInfo> {
        &self.class_info
    }

    pub(crate) fn cells_per_runtime(&self) -> &HashMap<RuntimeId, Vec<Sri>> {
        &self.cells_per_runtime
    }

    /// No retry here — transient read failures bubble up to `place_cells`,
    /// which retries the entire begin→read→commit cycle with a fresh tx.
    async fn read(db: &DbClient, tx_id: TxId, request: &PlacementRequest) -> Result<Self> {
        let execs = exec_registry::list_execs(db, tx_id)
            .await
            .map_err(|err| sorg_common::custom_err!("failed to read exec registry: {err}"))?;

        // A dead node lingers in the exec registry until introspection or
        // hygiene removes it; placing onto it would only time out and roll
        // back. Drop execs whose liveness lease has gone silent past the same
        // deadline hygiene uses, and execs with no lease row at all — every
        // live node leases, so absence means dead or not yet ready.
        let leases: HashMap<RuntimeId, (u64, u64)> = node_lease::list_leases_in_tx(db, tx_id)
            .await
            .map_err(|err| sorg_common::custom_err!("failed to read node leases: {err}"))?
            .into_iter()
            .map(|(id, lease)| (id, (lease.seq, lease.ttl_ms)))
            .collect();
        let now_ms = u64::try_from(
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(u64::MAX);
        let margin_ms =
            u64::try_from(SupervisionTiming::default().margin.as_millis()).unwrap_or(u64::MAX);
        let execs = drop_stale_execs(execs, &leases, now_ms, margin_ms);

        let mut class_info = HashMap::new();
        for cell in request.cells() {
            if let CellConfig::Wasm { ref class } = cell.config
                && !class_info.contains_key(class)
                && let Some(info) = class_registry::get_class_info_in_tx(db, tx_id, class).await?
            {
                class_info.insert(class.clone(), info);
            }
        }

        let all_cells = list_placements_in_tx(db, tx_id)
            .await
            .map_err(|err| sorg_common::custom_err!("failed to read placements: {err}"))?;

        let mut cells_per_runtime: HashMap<RuntimeId, Vec<Sri>> = HashMap::new();
        for entry in all_cells {
            if let PlacementKind::Wasm { ref runtime } = entry.kind {
                cells_per_runtime
                    .entry(runtime.id())
                    .or_default()
                    .push(entry.sri);
            }
        }

        Ok(Self {
            execs,
            class_info,
            cells_per_runtime,
        })
    }
}

/// A placement decision for a single cell: the cell and its assigned runtime.
pub(crate) struct CellPlacement {
    cell: CellDeployment,
    runtime: ExecRuntimeInfo,
}

impl CellPlacement {
    pub(crate) fn new(cell: CellDeployment, runtime: ExecRuntimeInfo) -> Self {
        Self { cell, runtime }
    }

    pub(crate) fn into_inner(self) -> (CellDeployment, ExecRuntimeInfo) {
        (self.cell, self.runtime)
    }
}

use crate::event_loop::Runtime;

impl Runtime {
    /// Placement is decided inside a committed read-tx, but mechanical loading
    /// happens after — a runtime can leave between the two. This is by design:
    /// the load will fail and the caller handles the error (app rollback / standalone error).
    ///
    /// Capacity enforcement assumes a single orchestrator writer. The placement tx is
    /// read-only, so two concurrent deploys both observe the same "runtime empty" snapshot,
    /// both commit without OCC conflict, and both place a cell on the same capacity-1
    /// runtime. Additionally, `cells_per_runtime` only counts `PlacementKind::Wasm` entries —
    /// `PlacementKind::Placeholder` (written by `claim_placement` before the load completes)
    /// is invisible to the capacity check, so in-flight concurrent deploys are not counted
    /// toward occupancy. Both limitations are benign with a single orchestrator instance.
    pub(crate) async fn place_cells(
        &self,
        request: PlacementRequest,
    ) -> std::result::Result<Vec<CellPlacement>, DeploymentError> {
        let db = DbClient::new(&self.session);

        for attempt in 1..=MAX_COMMIT_RETRIES {
            let tx_id = db
                .send(tx_begin::Request::routed(placement_scope()))
                .await
                .map_err(|err| {
                    DeploymentError::Internal(format!("failed to begin read tx: {err}"))
                })?
                .map_err(|err| {
                    DeploymentError::Internal(format!("failed to begin read tx: {}", err.message))
                })?
                .id;

            let context = PlacementContext::read(&db, tx_id, &request)
                .await
                .map_err(|err| DeploymentError::Internal(err.to_string()))?;
            let placements = decide_cell_placement(&request, &context)?;

            let commit_result = db.send(tx_commit::Request { id: tx_id }).await;
            match commit_result {
                Ok(Ok(_)) => return Ok(placements),
                Ok(Err(err)) => {
                    debug!(
                        "read tx commit rejected (attempt {attempt}/{MAX_COMMIT_RETRIES}): {}",
                        err.message
                    );
                }
                Err(err) => {
                    debug!("read tx commit failed (attempt {attempt}/{MAX_COMMIT_RETRIES}): {err}");
                }
            }
        }

        Err(DeploymentError::Internal(format!(
            "cell placement failed: read tx commit failed after {MAX_COMMIT_RETRIES} attempts"
        )))
    }
}

/// Drops execs whose liveness lease has gone stale or is missing. `seq` is
/// the writer's wall-clock time in millis at its last renewal, so a node
/// silent past its own declared ttl (plus the cluster margin) has almost
/// certainly died — placing a cell there would only time out on the deploy
/// and roll back. A `seq` ahead of the reader's clock (skew) counts as fresh,
/// never dropped. Every live node leases, so an exec with no lease row is
/// dropped too (dead with the row purged, or not yet clock-synced).
fn drop_stale_execs(
    execs: Vec<ExecRuntimeInfo>,
    leases: &HashMap<RuntimeId, (u64, u64)>,
    now_ms: u64,
    margin_ms: u64,
) -> Vec<ExecRuntimeInfo> {
    execs
        .into_iter()
        .filter(|exec| match leases.get(&exec.id()) {
            Some((seq, ttl_ms)) => now_ms.saturating_sub(*seq) <= ttl_ms.saturating_add(margin_ms),
            None => false,
        })
        .collect()
}

fn decide_cell_placement(
    request: &PlacementRequest,
    context: &PlacementContext,
) -> std::result::Result<Vec<CellPlacement>, DeploymentError> {
    if context.execs().is_empty() {
        return Err(DeploymentError::NoRuntimesAvailable);
    }

    let embedded_nodes: HashSet<RuntimeId> = context
        .execs()
        .iter()
        .filter(|e| e.runtime_kind().is_embedded())
        .map(ExecRuntimeInfo::id)
        .collect();

    // Seed per-runtime load from cells already deployed across the fleet so the
    // spread balances the resulting distribution, not just this batch.
    let existing_load: HashMap<RuntimeId, usize> = context
        .cells_per_runtime()
        .iter()
        .map(|(rt_id, cells)| (*rt_id, cells.len()))
        .collect();

    let cell_mappings = preprocessing::preprocess(request.cells(), context);
    let bindings = match triage::triage(cell_mappings, &embedded_nodes, &existing_load) {
        triage::TriageOutcome::Bound(bindings) => bindings,
        triage::TriageOutcome::Infeasible(infeasible) => {
            return Err(DeploymentError::Infeasible(infeasible));
        }
        triage::TriageOutcome::PlacementConflicts => {
            return Err(DeploymentError::PlacementConflicts);
        }
    };

    // O(1) lookups to recover, per binding, the original CellDeployment from its
    // SRI and the chosen runtime's full info from its id — avoiding O(n²) scans.
    let idx_by_sri: HashMap<&cell_protocol::Sri, usize> = request
        .cells()
        .iter()
        .enumerate()
        .map(|(i, c)| (&c.sri, i))
        .collect();
    let exec_by_id: HashMap<RuntimeId, &ExecRuntimeInfo> =
        context.execs().iter().map(|e| (e.id(), e)).collect();

    let placements = bindings
        .into_iter()
        .map(|binding| {
            let cell = request.cells()[idx_by_sri[&binding.sri]].clone();
            let runtime = exec_by_id[&binding.rt_id].clone();
            CellPlacement::new(cell, runtime)
        })
        .collect();

    Ok(placements)
}

#[cfg(test)]
mod tests {
    use cell_protocol::ExecutionCapabilities;

    use super::*;

    fn rt(n: u8) -> RuntimeId {
        zenoh_protocol::core::ZenohIdProto::try_from(&[n; 8][..])
            .unwrap()
            .into()
    }

    fn exec(id: RuntimeId) -> ExecRuntimeInfo {
        ExecRuntimeInfo::new(id, None, ExecutionCapabilities::default())
    }

    /// A node silent past its declared ttl (plus margin) is dropped; a
    /// freshly renewed node is kept; a node with no lease row at all is
    /// dropped — every live node leases, so absence is death evidence.
    #[test]
    fn drops_stale_and_leaseless_execs() {
        let (live, dead, leaseless) = (rt(1), rt(2), rt(3));
        let now_ms = 100_000;
        let margin_ms = 15_000;
        let leases = HashMap::from([
            (live, (95_000, 45_000)), // renewed 5s ago
            (dead, (20_000, 45_000)), // renewed 80s ago — past ttl+margin
        ]);

        let kept: Vec<RuntimeId> = drop_stale_execs(
            vec![exec(live), exec(dead), exec(leaseless)],
            &leases,
            now_ms,
            margin_ms,
        )
        .iter()
        .map(ExecRuntimeInfo::id)
        .collect();

        assert_eq!(kept, vec![live]);
    }

    /// A slow-renewing node declaring a larger ttl stays placeable through
    /// silence that would drop a default-ttl node.
    #[test]
    fn per_node_ttl_extends_the_freshness_deadline() {
        let node = rt(1);
        let leases = HashMap::from([(node, (20_000, 90_000))]); // 80s silent, 90s ttl
        let kept = drop_stale_execs(vec![exec(node)], &leases, 100_000, 15_000);
        assert_eq!(kept.len(), 1);
    }

    /// A lease `seq` ahead of the reader's clock (skew) is treated as fresh, so
    /// clock skew never wrongly drops a live node.
    #[test]
    fn future_lease_seq_counts_as_fresh() {
        let node = rt(1);
        let leases = HashMap::from([(node, (130_000, 45_000))]); // 30s ahead of now
        let kept = drop_stale_execs(vec![exec(node)], &leases, 100_000, 15_000);
        assert_eq!(kept.len(), 1);
    }
}
