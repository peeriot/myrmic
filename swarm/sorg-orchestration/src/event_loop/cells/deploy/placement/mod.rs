mod optimizer;
mod preprocessing;
mod triage;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime};

use cell_protocol::{ClassInfo, PlacementKind, RuntimeId, Sri};
use sorg_common::{
    CellConfig, CellDeployment, DeploymentError, ExecRuntimeInfo, class_registry, exec_registry,
    list_placements, node_lease, supervision::SupervisionTiming,
};
use tracing::debug;
use zenoh::Session;

use crate::Result;

/// One entry per gap between placement attempts, so the four attempts spend
/// 3.0s of patience in total.
///
/// The deploy deadline belongs to the client and this side never sees it: the
/// client puts it on the query and, once it expires, answers its own caller
/// with a timeout and discards whatever reply was on the way - including the
/// precise list of why each cell could not be placed. Patience spent here is
/// therefore spent blind, and past the caller's deadline it buys nothing while
/// replacing a usable diagnosis with an opaque one. The smallest deadline any
/// shipped caller sets is 15s, and this budget is a fifth of that floor, which
/// leaves the deploy's own work four fifths of the smallest budget it could be
/// facing. A caller that wants real patience has to spend it itself, where the
/// deadline is known.
///
/// One in-repo caller sits far below that floor and is the reason a test author
/// needs this paragraph: `sorg-tests` pins its client's query timeout to 3s, and
/// that is the client every `sorg-orchestration` and `sorg-client` integration
/// test deploys through. It equals this budget exactly, so a test deploying
/// through that client must not assert an artifact-blocked `Infeasible` - the
/// retries would eat the whole deadline and the test would be answered with a
/// query timeout instead. A test that needs that outcome has to bring a client
/// of its own.
const PLACEMENT_RETRY_BACKOFF: [Duration; 3] = [
    Duration::from_millis(500),
    Duration::from_millis(1000),
    Duration::from_millis(1500),
];

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

    async fn read(session: &Session, request: &PlacementRequest) -> Result<Self> {
        let execs = exec_registry::list_registered_execs(session)
            .await
            .map_err(|err| sorg_common::custom_err!("failed to read exec registry: {err}"))?;

        // A dead node lingers in the exec registry until introspection or
        // hygiene removes it; placing onto it would only time out and roll
        // back. Drop execs whose liveness lease has gone silent past the same
        // deadline hygiene uses, and execs with no lease row at all — every
        // live node leases, so absence means dead or not yet ready.
        let leases: HashMap<RuntimeId, (u64, u64)> = node_lease::list_leases(session)
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

        let class_names: Vec<String> = request
            .cells()
            .iter()
            .filter_map(|cell| match cell.config {
                CellConfig::Wasm { ref class } => Some(class.clone()),
                _ => None,
            })
            .collect();

        // Every read here is routed by its own scope, and the class registry is
        // why that matters. Routing picks a holder by maximising the located
        // scope's head and breaking a tie on rendezvous_hash(scope, id), so a
        // writer and a later reader land on the same node only when both
        // located the same scope. Registration writes these rows through the
        // class registry's scope; reading them inside a transaction anchored on
        // the placement scope drew an unrelated node. Every node replicates
        // `sorg`, so that read answered instead of failing - with no row at all
        // for a class registered moments earlier, or with a row whose artifact
        // set was one write behind. Both come back looking like a problem with
        // the class or the runtimes, and neither is one.
        let class_info = class_registry::get_class_infos(session, &class_names)
            .await
            .map_err(|err| sorg_common::custom_err!("failed to read class registry: {err}"))?;

        let all_cells = list_placements(session)
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
    /// Placement is decided from a snapshot taken before it, while the mechanical
    /// loading happens after - a runtime can leave between the two. This is by design:
    /// the load will fail and the caller handles the error (app rollback / standalone error).
    ///
    /// Capacity enforcement assumes a single orchestrator writer. Nothing here excludes
    /// a concurrent deploy, so two of them both observe the same "runtime empty" snapshot
    /// and both place a cell on the same capacity-1 runtime. Additionally,
    /// `cells_per_runtime` only counts `PlacementKind::Wasm` entries -
    /// `PlacementKind::Placeholder` (written by `claim_placement` before the load completes)
    /// is invisible to the capacity check, so in-flight concurrent deploys are not counted
    /// toward occupancy. Both limitations are benign with a single orchestrator instance.
    ///
    /// An outcome blocked only by a missing artifact is retried, because it is the one
    /// outcome a fresh read can change: an attempt re-runs all four routed reads, and a
    /// retry that reused the first read's answer could never change its own mind. Every
    /// other outcome, an unknown class included, is returned on the first attempt.
    pub(crate) async fn place_cells(
        &self,
        request: PlacementRequest,
    ) -> std::result::Result<Vec<CellPlacement>, DeploymentError> {
        const ATTEMPTS: usize = PLACEMENT_RETRY_BACKOFF.len() + 1;

        for (attempt, backoff) in PLACEMENT_RETRY_BACKOFF.into_iter().enumerate() {
            match self.place_cells_once(&request).await {
                Err(err) if err.blocked_only_by_missing_artifacts() => {
                    debug!(
                        "placement attempt {}/{ATTEMPTS} blocked by a missing artifact, \
                         retrying in {backoff:?}: {err}",
                        attempt + 1
                    );
                    tokio::time::sleep(backoff).await;
                }
                outcome => return outcome,
            }
        }

        // The last attempt is returned whatever it says, so the caller sees the
        // real placement outcome rather than an exhausted-retries error. Its
        // diagnosis is still worth a line: it is the only one that spent the
        // whole budget, and nothing downstream says the budget ran out.
        let outcome = self.place_cells_once(&request).await;
        if let Err(err) = &outcome
            && err.blocked_only_by_missing_artifacts()
        {
            debug!(
                "placement still blocked by a missing artifact after {ATTEMPTS} attempts: {err}"
            );
        }

        outcome
    }

    async fn place_cells_once(
        &self,
        request: &PlacementRequest,
    ) -> std::result::Result<Vec<CellPlacement>, DeploymentError> {
        let context = PlacementContext::read(&self.session, request)
            .await
            .map_err(|err| DeploymentError::Internal(err.to_string()))?;

        decide_cell_placement(request, &context)
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

    // A class the registry read did not return is not a per-runtime property:
    // no runtime can host it, and reporting it as every runtime lacking an
    // artifact describes the wrong thing. Name the class instead, before any
    // runtime is considered. Checked after the empty-registry case, because a
    // swarm with no runtimes cannot run the deploy whatever the class is.
    let unknown_class = request.cells().iter().find_map(|cell| match cell.config {
        CellConfig::Wasm { ref class } if !context.class_info().contains_key(class) => Some(class),
        _ => None,
    });
    if let Some(class) = unknown_class {
        return Err(DeploymentError::UnknownClass {
            class: class.clone(),
        });
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
