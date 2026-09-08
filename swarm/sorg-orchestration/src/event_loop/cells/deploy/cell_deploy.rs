use std::collections::HashMap;

use cell_protocol::Gen;
use cell_protocol::{PlacementEntry, PlacementKind, RuntimeId, Sri};
use sorg_common::{
    CellDeployment, CellFailure, CellFailureKind, DeployRequest, DeployResponse,
    DeployedCell as DeploymentResult, DeploymentError, FenceOutcome, PlacementClaimOutcome,
    SorgPayload, bail, claim_placement, commit_placement, get_placement, list_placements,
    remove_placement, zenoh_err,
};
use tracing::{debug, warn};
use zenoh::query::Query;

use crate::Result;
use crate::event_loop::Runtime;

use super::placement::PlacementRequest;
use super::{cell_failure_kind, reply_deployment_err};

/// Tracks what a deploy has claimed so a failure part-way through can be undone.
struct DeployTransaction {
    /// Every SRI this deploy claimed, with the generation minted for it. The
    /// generation fences the rollback: only rows of this incarnation go.
    claimed: Vec<(Sri, Gen)>,
    deployed: Vec<(Sri, PlacementKind)>,
    wrote_specs: Vec<Sri>,
}

impl DeployTransaction {
    fn new() -> Self {
        Self {
            claimed: Vec::new(),
            deployed: Vec::new(),
            wrote_specs: Vec::new(),
        }
    }

    async fn rollback(&self, rt: &Runtime) {
        let gens: HashMap<Sri, Gen> = self.claimed.iter().copied().collect();
        for (sri, kind) in &self.deployed {
            rt.teardown_cell_on_exec(sri, gens[sri], kind).await;
        }
        for (sri, gen_id) in &self.claimed {
            // A concurrent undeploy can have released this SRI and a redeploy
            // reclaimed it; then every record under it is the successor's.
            let row = match get_placement(&rt.session, sri).await {
                Ok(row) => row,
                Err(err) => {
                    warn!("rollback: placement read for '{sri}' failed: {err}");
                    continue;
                }
            };
            if let Some(entry) = &row
                && entry.gen_id != *gen_id
            {
                debug!(
                    "rollback: '{sri}' is held by generation {}; leaving its records",
                    entry.gen_id
                );
                continue;
            }
            // Cells run `#[init]` before the batch commits, so a rolled-back
            // cell may already own gateway routes and assets — also one whose
            // deploy failed after its init committed (a timeout, say). With
            // the placement already gone, the undeploy that took it has
            // released them, and a successor may be declaring its own.
            // Never the restart spec from here: it may predate this deploy (a
            // restart replays an existing spec), and erasing it on a failed
            // attempt would cancel the restart for good. Specs this deploy
            // wrote itself are erased just below.
            if row.is_some() {
                rt.release_cell_resources(sri, false).await;
            }
            if self.wrote_specs.contains(sri)
                && let Err(err) = sorg_common::root_restart::erase_spec(&rt.session, sri).await
            {
                warn!("rollback: failed to erase restart spec '{sri}': {err}");
            }
            // The placement is the claim: while it stands no successor can
            // start writing rows of its own, so it goes after everything a
            // successor could otherwise share. The instance row follows — its
            // erase is refused while a same-generation placement exists.
            match remove_placement(&rt.session, sri, *gen_id).await {
                Ok(FenceOutcome::Applied) => {}
                Ok(outcome) => debug!("rollback: placement '{sri}' not removed: {outcome:?}"),
                Err(err) => warn!("rollback: failed to remove placement '{sri}': {err}"),
            }
            // Written by the exec at init, or by the orchestrator for embedded
            // cells; absent when the deploy never got that far.
            match sorg_common::instance_registry::erase_instance(&rt.session, sri, *gen_id).await {
                Ok(FenceOutcome::Applied | FenceOutcome::Absent) => {}
                Ok(outcome) => debug!("rollback: instance '{sri}' not erased: {outcome:?}"),
                Err(err) => warn!("rollback: failed to erase instance '{sri}': {err}"),
            }
        }
    }
}

struct DeployedCell {
    sri: Sri,
    runtime: RuntimeId,
    kind: PlacementKind,
}

impl Runtime {
    pub(in crate::event_loop) async fn handle_deploy_cell_query(&self, query: Query) -> Result<()> {
        let Some(payload) = query.payload() else {
            bail!("cell deploy query without payload");
        };
        let request = DeployRequest::from_payload(payload, "orch: deser deploy request")?;

        let mut txn = DeployTransaction::new();
        match self.execute_deploy(&mut txn, request.cells).await {
            Ok(cells) => {
                let response = DeployResponse { cells };
                let payload = response.to_payload()?;
                query
                    .reply(query.key_expr(), payload)
                    .await
                    .map_err(|zen_err| {
                        zenoh_err!("orch failed to reply to cell deploy query", zen_err)
                    })?;
                Ok(())
            }
            Err(err) => {
                txn.rollback(self).await;
                reply_deployment_err(&query, err).await;
                Ok(())
            }
        }
    }

    /// Deploys a batch of cells atomically. A single CLI deploy or a runtime
    /// spawn is a batch of one; an app bundle is many. All-or-nothing: any
    /// failure rolls the whole batch back.
    async fn execute_deploy(
        &self,
        txn: &mut DeployTransaction,
        cells: Vec<CellDeployment>,
    ) -> std::result::Result<Vec<DeploymentResult>, DeploymentError> {
        if cells.is_empty() {
            return Err(DeploymentError::EmptyDeployment);
        }
        self.reject_duplicate_app_names(&cells).await?;

        // Deploy admission mints each instance's generation from the
        // session's HLC — the same clock ordering the db uses. The placement
        // row carrying it is the liveness anchor for that instance.
        let gen_ids: HashMap<Sri, Gen> = cells
            .iter()
            .map(|c| (c.sri, Gen::from_timestamp(&self.session.new_timestamp())))
            .collect();

        // Resolve each cell's app (explicit on the request, else inherited from
        // the spawning parent) and claim its SRI with a placeholder carrying it.
        let mut apps: HashMap<Sri, Option<String>> = HashMap::with_capacity(cells.len());
        for cell in &cells {
            let app = self.resolve_app(cell).await?;
            let placeholder = PlacementEntry {
                sri: cell.sri,
                kind: PlacementKind::Placeholder,
                app: app.clone(),
                gen_id: gen_ids[&cell.sri],
            };
            match claim_placement(&self.session, placeholder)
                .await
                .map_err(|err| DeploymentError::Internal(err.to_string()))?
            {
                PlacementClaimOutcome::Claimed => txn.claimed.push((cell.sri, gen_ids[&cell.sri])),
                PlacementClaimOutcome::AlreadyExists => {
                    return Err(DeploymentError::DuplicateSri { sri: cell.sri });
                }
            }
            apps.insert(cell.sri, app);
        }

        // Roots (no parent) with an enabled policy get a restart spec — the
        // full deployment, replayed verbatim after a qualifying death. Captured
        // before `place_and_deploy` consumes `cells`; written only once the
        // whole batch has committed, so a rolled-back deploy leaves none.
        let root_specs: Vec<CellDeployment> = cells
            .iter()
            .filter(|c| c.lineage.parent.is_none() && c.restart.is_enabled())
            .cloned()
            .collect();

        let deployed = self.place_and_deploy(txn, cells, &gen_ids).await?;

        // A placeholder that no longer carries this deploy's generation was
        // overtaken while the cell loaded — undeployed, or undeployed and
        // redeployed. Failing the batch hands the loaded cell to the rollback,
        // so a stale deploy can never publish itself over the newer state.
        let mut superseded = Vec::new();
        for d in &deployed {
            let entry = PlacementEntry {
                sri: d.sri,
                kind: d.kind.clone(),
                app: apps.remove(&d.sri).flatten(),
                gen_id: gen_ids[&d.sri],
            };
            match commit_placement(&self.session, entry)
                .await
                .map_err(|err| DeploymentError::Internal(err.to_string()))?
            {
                FenceOutcome::Applied => {}
                outcome => {
                    warn!(
                        "deploy: placement of '{}' not committed: {outcome:?}",
                        d.sri
                    );
                    superseded.push(CellFailure {
                        cell: d.sri,
                        runtime: d.runtime,
                        kind: CellFailureKind::Superseded,
                    });
                }
            }
        }
        if !superseded.is_empty() {
            return Err(DeploymentError::DeploymentFailed(superseded));
        }

        for spec in &root_specs {
            sorg_common::root_restart::write_spec(&self.session, spec)
                .await
                .map_err(|err| DeploymentError::Internal(err.to_string()))?;
            txn.wrote_specs.push(spec.sri);
        }
        Ok(deployed
            .into_iter()
            .map(|cell| {
                // The scheduler proposes a runtime for every request, but a
                // native bridge ignores it and commits a Bridge placement on
                // the orchestrator. Report the committed placement, never the
                // scheduler candidate.
                let runtime = match cell.kind {
                    PlacementKind::Wasm { runtime } => Some(runtime.id()),
                    PlacementKind::Bridge { .. } => None,
                    PlacementKind::Placeholder => {
                        unreachable!("deployed cell cannot be a placeholder")
                    }
                };
                DeploymentResult {
                    sri: cell.sri,
                    runtime,
                }
            })
            .collect())
    }

    /// A cell's app is what it declares, or — for a spawned cell that declares
    /// none — the app of the parent that spawned it, so a whole spawn tree ends
    /// up under one app name.
    async fn resolve_app(
        &self,
        cell: &CellDeployment,
    ) -> std::result::Result<Option<String>, DeploymentError> {
        if let Some(app) = &cell.app {
            return Ok(Some(app.clone()));
        }
        match cell.lineage.parent {
            Some(parent) => Ok(get_placement(&self.session, &parent)
                .await
                .map_err(|err| DeploymentError::Internal(err.to_string()))?
                .and_then(|parent_entry| parent_entry.app)),
            None => Ok(None),
        }
    }

    /// Best-effort guard against two apps sharing a name. Only root cells (no
    /// parent) introduce a name; spawned cells inherit and are exempt. A restart
    /// re-deploys an already-admitted root — one with a persisted restart spec —
    /// which legitimately shares its app name with live siblings, so it is not a
    /// collision; a fresh deploy reusing an in-use name still is.
    async fn reject_duplicate_app_names(
        &self,
        cells: &[CellDeployment],
    ) -> std::result::Result<(), DeploymentError> {
        let roots: Vec<&CellDeployment> = cells
            .iter()
            .filter(|c| c.lineage.parent.is_none())
            .collect();
        let mut names: Vec<&String> = roots.iter().filter_map(|c| c.app.as_ref()).collect();
        names.sort();
        names.dedup();
        if names.is_empty() {
            return Ok(());
        }
        let existing = list_placements(&self.session)
            .await
            .map_err(|err| DeploymentError::Internal(err.to_string()))?;
        for name in names {
            if !existing.iter().any(|e| e.app.as_ref() == Some(name)) {
                continue;
            }
            if self.all_claimants_are_restarts(&roots, name).await? {
                continue;
            }
            return Err(DeploymentError::DuplicateAppName { name: name.clone() });
        }
        Ok(())
    }

    /// Whether every incoming root claiming `name` already has a restart spec —
    /// i.e. the batch is re-admitting known roots (a restart), not introducing a
    /// fresh app under a name already in use.
    async fn all_claimants_are_restarts(
        &self,
        roots: &[&CellDeployment],
        name: &str,
    ) -> std::result::Result<bool, DeploymentError> {
        for cell in roots.iter().filter(|c| c.app.as_deref() == Some(name)) {
            if sorg_common::root_restart::get_spec(&self.session, &cell.sri)
                .await
                .map_err(|err| DeploymentError::Internal(err.to_string()))?
                .is_none()
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Places the whole batch against one occupancy snapshot, then deploys every
    /// cell concurrently. Succeeds only if all cells deploy.
    async fn place_and_deploy(
        &self,
        txn: &mut DeployTransaction,
        cells: Vec<CellDeployment>,
        gen_ids: &HashMap<Sri, Gen>,
    ) -> std::result::Result<Vec<DeployedCell>, DeploymentError> {
        let placements = self.place_cells(PlacementRequest::for_cells(cells)).await?;

        let futs = placements.into_iter().map(|placement| {
            let (cell, runtime) = placement.into_inner();
            let sri = cell.sri;
            let gen_id = gen_ids
                .get(&sri)
                .copied()
                .unwrap_or_else(|| Gen::from_timestamp(&self.session.new_timestamp()));
            async move {
                match self
                    .deploy_cell(
                        &sri,
                        cell.config,
                        &runtime,
                        gen_id,
                        cell.lineage,
                        cell.arguments,
                    )
                    .await
                {
                    Ok(kind) => Ok(DeployedCell {
                        sri,
                        runtime: runtime.id(),
                        kind,
                    }),
                    Err(err) => Err(CellFailure {
                        cell: sri,
                        runtime: runtime.id(),
                        kind: cell_failure_kind(err),
                    }),
                }
            }
        });

        let results = futures::future::join_all(futs).await;

        let mut deployed = Vec::new();
        let mut failures = Vec::new();
        for result in results {
            match result {
                Ok(d) => {
                    txn.deployed.push((d.sri, d.kind.clone()));
                    deployed.push(d);
                }
                Err(f) => failures.push(f),
            }
        }

        if failures.is_empty() {
            Ok(deployed)
        } else {
            Err(DeploymentError::DeploymentFailed(failures))
        }
    }
}
