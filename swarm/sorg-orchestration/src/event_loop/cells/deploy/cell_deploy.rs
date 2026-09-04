use std::collections::HashMap;

use cell_protocol::Gen;
use cell_protocol::{PlacementEntry, PlacementKind, Sri};
use sorg_common::{
    CellDeployment, CellFailure, DeployRequest, DeploymentError, PlacementClaimOutcome,
    SorgPayload, bail, claim_placement, commit_placement, get_placement, list_placements,
    remove_placement, zenoh_err,
};
use tracing::warn;
use zenoh::query::Query;

use crate::Result;
use crate::event_loop::Runtime;

use super::placement::PlacementRequest;
use super::{cell_failure_kind, reply_deployment_err};

/// Tracks what a deploy has claimed so a failure part-way through can be undone.
struct DeployTransaction {
    claimed_sris: Vec<Sri>,
    deployed: Vec<(Sri, PlacementKind)>,
    wrote_specs: Vec<Sri>,
}

impl DeployTransaction {
    fn new() -> Self {
        Self {
            claimed_sris: Vec::new(),
            deployed: Vec::new(),
            wrote_specs: Vec::new(),
        }
    }

    async fn rollback(&self, rt: &Runtime) {
        for (sri, kind) in &self.deployed {
            rt.teardown_cell_on_exec(sri, kind).await;
        }
        for sri in &self.claimed_sris {
            if let Err(err) = remove_placement(&rt.session, sri).await {
                warn!("rollback: failed to remove placement '{sri}': {err}");
            }
        }
        // Deployed cells may already have instance rows (written by the exec
        // or, for embedded, by the orchestrator itself).
        for (sri, _) in &self.deployed {
            if let Err(err) = sorg_common::instance_registry::erase_instance(&rt.session, sri).await
            {
                warn!("rollback: failed to erase instance '{sri}': {err}");
            }
        }
        // Restart specs written for this batch must not outlive a rollback.
        for sri in &self.wrote_specs {
            if let Err(err) = sorg_common::root_restart::erase_spec(&rt.session, sri).await {
                warn!("rollback: failed to erase restart spec '{sri}': {err}");
            }
        }
    }
}

struct DeployedCell {
    sri: Sri,
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
            Ok(()) => {
                query
                    .reply(query.key_expr(), vec![])
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
    ) -> std::result::Result<(), DeploymentError> {
        if cells.is_empty() {
            return Err(DeploymentError::EmptyDeployment);
        }
        // An unregistered class surfaces as an Infeasible placement below (no
        // exec can host it) rather than being pre-checked here.
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
                PlacementClaimOutcome::Claimed => txn.claimed_sris.push(cell.sri),
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

        for d in &deployed {
            let entry = PlacementEntry {
                sri: d.sri,
                kind: d.kind.clone(),
                app: apps.remove(&d.sri).flatten(),
                gen_id: gen_ids[&d.sri],
            };
            commit_placement(&self.session, entry)
                .await
                .map_err(|err| DeploymentError::Internal(err.to_string()))?;
        }

        for spec in &root_specs {
            sorg_common::root_restart::write_spec(&self.session, spec)
                .await
                .map_err(|err| DeploymentError::Internal(err.to_string()))?;
            txn.wrote_specs.push(spec.sri);
        }
        Ok(())
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
                    Ok(kind) => Ok(DeployedCell { sri, kind }),
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
