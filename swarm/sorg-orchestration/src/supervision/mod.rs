//! Orchestrator-side supervision: watches node leases and, as leader, runs
//! row hygiene for dead nodes (spec §7). Level-triggered — every pass
//! recomputes from db state, so an orchestrator that was down during a death
//! acts on its next scan. Cells die individually: each is declared dead once
//! its node has been silent past the cell's own deadline (default: the
//! node's declared lease ttl + margin); the node itself is torn down only
//! after all its cells are gone.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use cell_protocol::{CellInstance, Gen, PlacementEntry, PlacementKind, RuntimeId, Sri};
use sorg_common::root_death::RootDeath;
use sorg_common::supervision::{
    ExpiryGate, LeaseTracker, RestartBudget, SupervisionTiming, jittered,
};
use sorg_common::{
    CellDeployment, DeployRequest, LostReason, deploy_cells, instance_registry, list_placements,
    node_lease, remove_placement, root_death, root_restart, should_restart,
};
use tracing::{debug, info, warn};
use zenoh::Session;

use crate::state::State;

/// A `cell_lost { node_lost }` notification hygiene owes a live parent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CellLostNote {
    pub parent: Sri,
    pub cell: Sri,
    pub cell_gen: Gen,
    pub local_name: Option<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct HygienePlan {
    pub notes: Vec<CellLostNote>,
    /// Ordered: placement rows are released before instance rows —
    /// instance-erase refuses while a placement row exists.
    pub release_cells: Vec<Sri>,
    pub erase_instances: Vec<Sri>,
    /// Nodes whose every placed cell is being released: only then may the
    /// exec row and (last) the lease row go — while any placement row
    /// remains, the expired lease must survive as evidence.
    pub teardown_nodes: Vec<RuntimeId>,
}

impl HygienePlan {
    fn is_empty(&self) -> bool {
        self.notes.is_empty() && self.release_cells.is_empty() && self.teardown_nodes.is_empty()
    }
}

fn placement_node(entry: &PlacementEntry) -> Option<RuntimeId> {
    match &entry.kind {
        PlacementKind::Wasm { runtime } => Some(runtime.id()),
        PlacementKind::Bridge { .. } | PlacementKind::Placeholder => None,
    }
}

/// Placed nodes with no lease row at all. Every live node leases, and a dead
/// node's last renewal survives only its retention, so sustained absence of
/// the row is death evidence: the node died long enough ago for the row to
/// purge, or this observer started after it did. Silence is measured from the
/// first miss (erring late); nodes re-lease on reconnect, so a survivable
/// blip refills the row well inside the gate's threshold.
fn lease_missing(placed: &[RuntimeId], leased: &HashSet<RuntimeId>) -> Vec<RuntimeId> {
    let mut nodes: Vec<RuntimeId> = placed
        .iter()
        .filter(|node| !leased.contains(node))
        .copied()
        .collect();
    nodes.sort_by_key(ToString::to_string);
    nodes.dedup();
    nodes
}

/// Plans hygiene from per-node silence. A cell is due once its node has been
/// silent for the cell's own deadline: edge-declared (floored at twice the
/// renewal period so it cannot sit inside normal renewal jitter), defaulting
/// to the hosting node's declared ttl plus the margin. Notes cross only the
/// death boundary: a parent that is itself due gets nothing (its own parent
/// is told about *it*), and detached edges get nothing. Every due cell is
/// cleaned up, detached included.
pub(crate) fn plan_hygiene(
    silence: &HashMap<RuntimeId, Duration>,
    teardown_ready: &[RuntimeId],
    cells: &[PlacementEntry],
    instances: &[CellInstance],
    node_ttls: &HashMap<RuntimeId, Duration>,
    timing: &SupervisionTiming,
) -> HygienePlan {
    let min_deadline = timing.renew * 2;

    let placements: HashMap<Sri, RuntimeId> = cells
        .iter()
        .filter_map(|c| placement_node(c).map(|node| (c.sri, node)))
        .collect();
    let by_sri: HashMap<Sri, &CellInstance> = instances.iter().map(|i| (i.sri, i)).collect();

    let deadline = |sri: &Sri, node: RuntimeId| {
        by_sri
            .get(sri)
            .and_then(|i| i.lineage.deadline_ms)
            .map_or_else(
                || node_ttls.get(&node).copied().unwrap_or(timing.ttl) + timing.margin,
                |ms| Duration::from_millis(ms).max(min_deadline),
            )
    };
    let due: HashSet<Sri> = cells
        .iter()
        .filter(|c| {
            placement_node(c).is_some_and(|node| {
                silence
                    .get(&node)
                    .is_some_and(|silent| *silent >= deadline(&c.sri, node))
            })
        })
        .map(|c| c.sri)
        .collect();

    let mut plan = HygienePlan::default();
    for cell in cells {
        if !due.contains(&cell.sri) {
            continue;
        }
        plan.release_cells.push(cell.sri);
        plan.erase_instances.push(cell.sri);

        let Some(instance) = by_sri.get(&cell.sri) else {
            continue;
        };
        if instance.lineage.detached {
            continue;
        }
        let Some(parent) = instance.lineage.parent else {
            continue;
        };
        // Owed only to a parent that is not itself being declared dead. A
        // doomed-but-not-yet-due parent still gets one: mailboxes are
        // db-backed and dead letters are harmless.
        if due.contains(&parent) {
            continue;
        }
        if !placements.contains_key(&parent) {
            continue;
        }
        plan.notes.push(CellLostNote {
            parent,
            cell: cell.sri,
            cell_gen: instance.gen_id,
            local_name: instance.lineage.local_name.clone(),
        });
    }

    plan.teardown_nodes = teardown_ready
        .iter()
        .filter(|node| {
            cells
                .iter()
                .filter(|c| placement_node(c) == Some(**node))
                .all(|c| due.contains(&c.sri))
        })
        .copied()
        .collect();
    plan
}

/// Watches node leases and runs hygiene as leader. Spawned once per
/// orchestrator; non-leaders keep observing leases (warm tracker) but act on
/// nothing.
pub(crate) fn spawn_lease_watcher(
    session: Session,
    state: State,
    timing: SupervisionTiming,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tracker = LeaseTracker::new();
        let mut gate = ExpiryGate::new(timing.margin);
        // A missing lease row measures silence from the first miss; the
        // gate's threshold is only its teardown deadline.
        let mut absence_gate = ExpiryGate::new(timing.ttl + timing.margin);
        let mut budget = RestartBudget::new();
        let mut sweep = RestartSweep::default();
        let mut tick: u64 = 0;
        loop {
            tick += 1;
            tokio::time::sleep(jittered(timing.verify, tick)).await;

            let leases = match node_lease::list_leases(&session).await {
                Ok(leases) => leases,
                Err(err) => {
                    debug!("lease scan failed: {err}");
                    continue;
                }
            };
            let now = Instant::now();
            for (id, lease) in &leases {
                tracker.observe(*id, lease.seq, Duration::from_millis(lease.ttl_ms), now);
            }
            let leased: HashSet<RuntimeId> = leases.iter().map(|(id, _)| *id).collect();

            let cells = match list_placements(&session).await {
                Ok(cells) => cells,
                Err(err) => {
                    debug!("hygiene: cell scan failed: {err}");
                    continue;
                }
            };
            let placed: Vec<RuntimeId> = cells.iter().filter_map(placement_node).collect();

            // Per-node silence, on this observer's own clock. An observed
            // lease measures from the last seq advance; a missing lease row
            // from when it was first missed.
            let mut silence: HashMap<RuntimeId, Duration> = HashMap::new();
            for node in &placed {
                if let Some(stale) = tracker.stale_for(*node, now) {
                    silence.insert(*node, stale);
                }
            }
            let mut teardown_ready = gate.ready(&tracker.expired(now), now);

            for (node, silent) in absence_gate.silences(&lease_missing(&placed, &leased), now) {
                // A prior observation's silence is the better (longer-armed)
                // evidence; the gate only covers rows this observer never saw.
                silence.entry(node).or_insert(silent);
                if silent >= timing.ttl + timing.margin {
                    teardown_ready.push(node);
                }
            }
            teardown_ready.sort_by_key(ToString::to_string);
            teardown_ready.dedup();

            let is_leader = { state.lock().await.is_leader().unwrap_or(false) };
            if !is_leader {
                continue;
            }

            // Hygiene runs only when a node looks dead (nothing is due below
            // the deadline floor); restart processing runs every leader pass,
            // since a crash death carries no node silence to gate on.
            let min_deadline = timing.renew * 2;
            if !teardown_ready.is_empty() || silence.values().any(|s| *s >= min_deadline) {
                run_hygiene(
                    &session,
                    &silence,
                    &teardown_ready,
                    &cells,
                    &mut tracker,
                    &timing,
                )
                .await;
            }

            process_root_restarts(&session, &mut budget, &mut sweep).await;
        }
    })
}

async fn run_hygiene(
    session: &Session,
    silence: &HashMap<RuntimeId, Duration>,
    teardown_ready: &[RuntimeId],
    cells: &[PlacementEntry],
    tracker: &mut LeaseTracker,
    timing: &SupervisionTiming,
) {
    let instances = match instance_registry::list_instances(session).await {
        Ok(instances) => instances,
        Err(err) => {
            warn!("hygiene: instance scan failed: {err}");
            return;
        }
    };

    let node_ttls: HashMap<RuntimeId, Duration> = cells
        .iter()
        .filter_map(placement_node)
        .filter_map(|node| tracker.ttl_of(node).map(|ttl| (node, ttl)))
        .collect();
    let plan = plan_hygiene(
        silence,
        teardown_ready,
        cells,
        &instances,
        &node_ttls,
        timing,
    );
    if plan.is_empty() {
        return;
    }
    info!(
        cells = plan.release_cells.len(),
        notes = plan.notes.len(),
        nodes = ?plan.teardown_nodes,
        "hygiene: cleaning up after dead cell(s)"
    );

    // A root lost to node death records a death signal, so the restart
    // processor handles node-loss uniformly with live-node deaths and the
    // reconciliation sweep doesn't mistake it for a deliberate removal.
    let instance_by_sri: HashMap<Sri, &CellInstance> =
        instances.iter().map(|i| (i.sri, i)).collect();
    for sri in &plan.erase_instances {
        if let Some(inst) = instance_by_sri
            .get(sri)
            .filter(|i| i.lineage.parent.is_none())
            && let Err(err) =
                root_death::record(session, *sri, inst.gen_id, LostReason::NodeLost).await
        {
            warn!("hygiene: recording root death '{sri}': {err}");
        }
    }

    // Notifications precede cleanup: once the rows are gone the next scan
    // cannot re-derive the notes, so a failed emission aborts this pass and
    // the level-triggered rescan retries everything. Parents tolerate the
    // resulting duplicates by design.
    let mut emissions_failed = false;
    for note in &plan.notes {
        let payload = sorg_common::CellLost {
            cell: note.cell,
            local_name: note.local_name.clone(),
            reason: sorg_common::LostReason::NodeLost,
        };
        if let Err(err) = sorg_common::emit_cell_lost(session, &note.parent, payload).await {
            warn!(
                "hygiene: cell_lost to '{parent}' failed: {err}",
                parent = note.parent
            );
            emissions_failed = true;
        }
    }
    if emissions_failed {
        return;
    }
    for sri in &plan.release_cells {
        if let Err(err) = remove_placement(session, sri).await {
            warn!("hygiene: releasing placement row '{sri}' failed: {err}");
        }
    }
    for sri in &plan.erase_instances {
        if let Err(err) = instance_registry::erase_instance(session, sri).await {
            debug!("hygiene: erasing instance row '{sri}': {err}");
        }
    }
    for id in &plan.teardown_nodes {
        if let Err(err) =
            sorg_common::exec_registry::deregister_exec_by_runtime_id(session, *id).await
        {
            warn!("hygiene: deregistering exec '{id}' failed: {err}");
        }
        // The lease row goes last: while any placement row still exists, the
        // expired lease must remain as evidence (missing-row rule).
        if let Err(err) = node_lease::delete_lease(session, *id).await {
            warn!("hygiene: deleting lease '{id}' failed: {err}");
        }
        tracker.forget(*id);
    }
}

/// Wall-clock budget for a self-issued restart deploy.
const RESTART_DEPLOY_TIMEOUT: Duration = Duration::from_secs(30);

/// Two-pass confirmation for the reconciliation sweep: a spec is dropped only
/// after looking orphaned (root gone, no death signal) on two consecutive
/// passes, so a brief redeploy window is never mistaken for an operator
/// removal.
#[derive(Debug, Default)]
struct RestartSweep {
    prev: HashSet<Sri>,
}

impl RestartSweep {
    fn confirm(&mut self, current: Vec<Sri>) -> Vec<Sri> {
        let cur: HashSet<Sri> = current.into_iter().collect();
        let confirmed = cur
            .iter()
            .filter(|s| self.prev.contains(s))
            .copied()
            .collect();
        self.prev = cur;
        confirmed
    }
}

/// The actions a restart pass resolves to, kept pure and separate from the I/O
/// that carries them out (mirroring [`plan_hygiene`]/[`run_hygiene`]).
#[derive(Debug, Default, PartialEq, Eq)]
struct RestartPlan {
    /// Roots to redeploy from their spec; the death signal is cleared on a
    /// successful redeploy (so a failed one is retried next pass).
    restart: Vec<Sri>,
    /// Specs to erase: a terminal death, a give-up, or a reconciled orphan.
    drop_specs: Vec<Sri>,
    /// Death signals to clear now: stale, unmatched by a spec, or accompanying
    /// a dropped spec.
    clear_deaths: Vec<Sri>,
}

/// Resolves restart actions from persisted specs and pending death signals.
/// Pure and deterministic given the db snapshot plus the leader-local budget
/// and sweep state. A death is acted on only once the dead instance's rows are
/// gone (so the redeploy claims a free SRI); a newer generation on the row
/// means the root already came back and the signal is stale.
fn plan_root_restarts(
    specs: &[CellDeployment],
    deaths: &[RootDeath],
    placements: &[PlacementEntry],
    instances: &[CellInstance],
    budget: &mut RestartBudget,
    sweep: &mut RestartSweep,
    now: Instant,
) -> RestartPlan {
    let spec_by_sri: HashMap<Sri, &CellDeployment> = specs.iter().map(|s| (s.sri, s)).collect();
    let placement_gen: HashMap<Sri, Gen> = placements.iter().map(|p| (p.sri, p.gen_id)).collect();
    let has_instance: HashSet<Sri> = instances.iter().map(|i| i.sri).collect();

    let mut plan = RestartPlan::default();
    for death in deaths {
        let sri = death.sri;
        let Some(spec) = spec_by_sri.get(&sri) else {
            // No policy for this root; nothing to restart.
            plan.clear_deaths.push(sri);
            continue;
        };
        match placement_gen.get(&sri) {
            // A newer generation already holds the row: the root is back and
            // this signal is stale.
            Some(cur) if *cur > death.gen_id => {
                plan.clear_deaths.push(sri);
                continue;
            }
            // The dead instance's placement is still being cleaned up; wait.
            Some(_) => continue,
            None => {}
        }
        if has_instance.contains(&sri) {
            // Placement gone but the instance row lingers; wait for its erase.
            continue;
        }

        let policy = &spec.restart;
        if should_restart(policy.restart_type, &death.reason) {
            // Hold off until the fixed inter-attempt delay has elapsed.
            if !budget.ready(&sri, Duration::from_millis(policy.delay_ms), now) {
                continue;
            }
            let window = Duration::from_millis(policy.window_ms);
            if budget.allow(sri, policy.max_restarts, window, now) {
                plan.restart.push(sri);
            } else {
                // Crash-loop budget exhausted: give up on this root.
                budget.forget(&sri);
                plan.drop_specs.push(sri);
                plan.clear_deaths.push(sri);
            }
        } else {
            // Terminal under policy: a clean stop under on-error, or a
            // terminate / cascade.
            budget.forget(&sri);
            plan.drop_specs.push(sri);
            plan.clear_deaths.push(sri);
        }
    }

    // A spec whose root is gone with no pending death signal is a deliberate
    // removal (operator undeploy leaves no signal). Confirmed across two passes.
    let death_sris: HashSet<Sri> = deaths.iter().map(|d| d.sri).collect();
    let orphaned: Vec<Sri> = specs
        .iter()
        .map(|s| s.sri)
        .filter(|sri| {
            !placement_gen.contains_key(sri)
                && !has_instance.contains(sri)
                && !death_sris.contains(sri)
        })
        .collect();
    for sri in sweep.confirm(orphaned) {
        budget.forget(&sri);
        plan.drop_specs.push(sri);
    }
    plan
}

/// Drives root auto-restart from persisted specs and pending death signals.
/// Leader-only and level-triggered like hygiene: it recovers whatever state it
/// finds each pass. Reads the db snapshot, plans with [`plan_root_restarts`],
/// then carries the plan out.
async fn process_root_restarts(
    session: &Session,
    budget: &mut RestartBudget,
    sweep: &mut RestartSweep,
) {
    let specs = match root_restart::list_specs(session).await {
        Ok(specs) => specs,
        Err(err) => {
            warn!("restart: spec scan failed: {err}");
            return;
        }
    };
    let deaths = match root_death::list(session).await {
        Ok(deaths) => deaths,
        Err(err) => {
            warn!("restart: death scan failed: {err}");
            return;
        }
    };
    if specs.is_empty() && deaths.is_empty() {
        return;
    }
    let placements = match list_placements(session).await {
        Ok(placements) => placements,
        Err(err) => {
            debug!("restart: placement scan failed: {err}");
            return;
        }
    };
    let instances = match instance_registry::list_instances(session).await {
        Ok(instances) => instances,
        Err(err) => {
            debug!("restart: instance scan failed: {err}");
            return;
        }
    };

    let plan = plan_root_restarts(
        &specs,
        &deaths,
        &placements,
        &instances,
        budget,
        sweep,
        Instant::now(),
    );

    let spec_by_sri: HashMap<Sri, &CellDeployment> = specs.iter().map(|s| (s.sri, s)).collect();
    for sri in &plan.restart {
        let Some(spec) = spec_by_sri.get(sri) else {
            continue;
        };
        info!(%sri, "restart: redeploying root");
        let request = DeployRequest::new(vec![(*spec).clone()]);
        match deploy_cells(session, request, RESTART_DEPLOY_TIMEOUT).await {
            Ok(()) => clear_death(session, sri).await,
            // Keep the signal; the next level-triggered pass retries.
            Err(err) => warn!(%sri, "restart: redeploy failed: {err}"),
        }
    }
    for sri in &plan.drop_specs {
        info!(%sri, "restart: dropping spec (terminal, give-up, or removed)");
        drop_spec(session, sri).await;
    }
    for sri in &plan.clear_deaths {
        clear_death(session, sri).await;
    }
}

async fn clear_death(session: &Session, sri: &Sri) {
    if let Err(err) = root_death::clear(session, sri).await {
        debug!("restart: clearing death signal '{sri}': {err}");
    }
}

async fn drop_spec(session: &Session, sri: &Sri) {
    if let Err(err) = root_restart::erase_spec(session, sri).await {
        warn!("restart: erasing spec '{sri}': {err}");
    }
}

#[cfg(test)]
mod tests {
    use cell_protocol::{ExecRuntimeInfo, ExecutionCapabilities};

    use super::*;

    fn rt(n: u8) -> RuntimeId {
        zenoh_protocol::core::ZenohIdProto::try_from(&[n; 8][..])
            .unwrap()
            .into()
    }

    fn sri(name: &str) -> Sri {
        cell_protocol::sri_of_path(name).unwrap().into()
    }

    fn entry(s: Sri, node: RuntimeId) -> PlacementEntry {
        PlacementEntry {
            sri: s,
            kind: PlacementKind::Wasm {
                runtime: ExecRuntimeInfo::new(node, None, ExecutionCapabilities::default()),
            },
            app: None,
            gen_id: Gen::from_parts(1, 1),
        }
    }

    fn instance(s: Sri, parent: Option<Sri>, detached: bool) -> CellInstance {
        CellInstance {
            sri: s,
            class_name: "c".into(),
            gen_id: Gen::from_parts(1, 1),
            lineage: cell_protocol::SpawnLineage {
                parent,
                parent_gen_id: parent.map(|_| Gen::from_parts(2, 1)),
                detached,
                local_name: Some("kid".into()),
                grace_ms: None,
                deadline_ms: None,
            },
        }
    }

    fn with_deadline(mut i: CellInstance, ms: u64) -> CellInstance {
        i.lineage.deadline_ms = Some(ms);
        i
    }

    fn silent(pairs: &[(RuntimeId, u64)]) -> HashMap<RuntimeId, Duration> {
        pairs
            .iter()
            .map(|(id, secs)| (*id, Duration::from_secs(*secs)))
            .collect()
    }

    const TIMING: SupervisionTiming = SupervisionTiming {
        renew: Duration::from_secs(10),
        ttl: Duration::from_secs(45),
        margin: Duration::from_secs(15),
        verify: Duration::from_secs(10),
    };

    fn no_ttls() -> HashMap<RuntimeId, Duration> {
        HashMap::new()
    }

    #[test]
    fn plans_cleanup_for_all_dead_cells_and_notes_only_boundary_edges() {
        let (dead_rt, live_rt) = (rt(1), rt(2));
        let (gp, p, c, d) = (sri("gp"), sri("p"), sri("c"), sri("d"));
        let cells = vec![
            entry(gp, live_rt),
            entry(p, dead_rt),
            entry(c, dead_rt),
            entry(d, dead_rt),
        ];
        let instances = vec![
            instance(gp, None, false),
            instance(p, Some(gp), false),
            instance(c, Some(p), false),
            instance(d, Some(gp), true), // detached child of gp
        ];

        let plan = plan_hygiene(
            &silent(&[(dead_rt, 61)]),
            &[dead_rt],
            &cells,
            &instances,
            &no_ttls(),
            &TIMING,
        );
        assert_eq!(plan.release_cells, vec![p, c, d]);
        assert_eq!(plan.erase_instances, vec![p, c, d]);
        assert_eq!(plan.notes.len(), 1);
        assert_eq!(plan.notes[0].parent, gp);
        assert_eq!(plan.notes[0].cell, p);
        assert_eq!(plan.notes[0].local_name.as_deref(), Some("kid"));
        assert_eq!(plan.teardown_nodes, vec![dead_rt]);
    }

    #[test]
    fn live_node_cells_are_untouched_and_empty_nodes_tear_down() {
        let live = rt(2);
        let cells = vec![entry(sri("a"), live)];
        let instances = vec![instance(sri("a"), None, false)];
        let plan = plan_hygiene(
            &silent(&[(rt(1), 61)]),
            &[rt(1)],
            &cells,
            &instances,
            &no_ttls(),
            &TIMING,
        );
        assert!(plan.notes.is_empty());
        assert!(plan.release_cells.is_empty());
        // A dead node hosting nothing has only its exec/lease rows left.
        assert_eq!(plan.teardown_nodes, vec![rt(1)]);
    }

    #[test]
    fn dead_parent_of_live_child_gets_cleaned_but_no_note() {
        // p@dead -> c@live: c is not hygiene's problem (fencing kills it);
        // p is dead so nobody notes p's loss of c.
        let (dead_rt, live_rt) = (rt(1), rt(2));
        let (p, c) = (sri("p"), sri("c"));
        let cells = vec![entry(p, dead_rt), entry(c, live_rt)];
        let instances = vec![instance(p, None, false), instance(c, Some(p), false)];

        let plan = plan_hygiene(
            &silent(&[(dead_rt, 61)]),
            &[dead_rt],
            &cells,
            &instances,
            &no_ttls(),
            &TIMING,
        );
        assert_eq!(plan.release_cells, vec![p]);
        assert!(plan.notes.is_empty());
    }

    #[test]
    fn note_skipped_when_parent_has_no_placement() {
        let dead_rt = rt(1);
        let (p, c) = (sri("p"), sri("c"));
        let cells = vec![entry(c, dead_rt)];
        let instances = vec![instance(c, Some(p), false)];

        let plan = plan_hygiene(
            &silent(&[(dead_rt, 61)]),
            &[dead_rt],
            &cells,
            &instances,
            &no_ttls(),
            &TIMING,
        );
        assert_eq!(plan.release_cells, vec![c]);
        assert!(plan.notes.is_empty());
    }

    #[test]
    fn short_deadline_declares_dead_before_the_cluster_default() {
        // Node silent 25s: below the 60s default, past a's 20s deadline.
        let (node, live_rt) = (rt(1), rt(2));
        let (gp, a, b) = (sri("gp"), sri("a"), sri("b"));
        let cells = vec![entry(gp, live_rt), entry(a, node), entry(b, node)];
        let instances = vec![
            instance(gp, None, false),
            with_deadline(instance(a, Some(gp), false), 20_000),
            instance(b, Some(gp), false),
        ];

        let plan = plan_hygiene(
            &silent(&[(node, 25)]),
            &[],
            &cells,
            &instances,
            &no_ttls(),
            &TIMING,
        );
        assert_eq!(plan.release_cells, vec![a]);
        assert_eq!(plan.notes.len(), 1);
        assert_eq!(plan.notes[0].cell, a);
        assert!(plan.teardown_nodes.is_empty());
    }

    #[test]
    fn long_deadline_keeps_rows_and_defers_node_teardown() {
        // Node silent 70s and gate-ready, but b rides out 300s before it is
        // declared dead; the node's lease must remain as evidence for b.
        let node = rt(1);
        let (a, b) = (sri("a"), sri("b"));
        let cells = vec![entry(a, node), entry(b, node)];
        let instances = vec![
            instance(a, None, false),
            with_deadline(instance(b, None, false), 300_000),
        ];

        let plan = plan_hygiene(
            &silent(&[(node, 70)]),
            &[node],
            &cells,
            &instances,
            &no_ttls(),
            &TIMING,
        );
        assert_eq!(plan.release_cells, vec![a]);
        assert!(plan.teardown_nodes.is_empty());

        let plan = plan_hygiene(
            &silent(&[(node, 301)]),
            &[node],
            &cells,
            &instances,
            &no_ttls(),
            &TIMING,
        );
        assert_eq!(plan.release_cells, vec![a, b]);
        assert_eq!(plan.teardown_nodes, vec![node]);
    }

    #[test]
    fn deadline_is_floored_at_twice_the_renewal_period() {
        // A 1ms deadline cannot fire inside normal renewal jitter.
        let node = rt(1);
        let a = sri("a");
        let cells = vec![entry(a, node)];
        let instances = vec![with_deadline(instance(a, None, false), 1)];

        let plan = plan_hygiene(
            &silent(&[(node, 19)]),
            &[],
            &cells,
            &instances,
            &no_ttls(),
            &TIMING,
        );
        assert!(plan.release_cells.is_empty());

        let plan = plan_hygiene(
            &silent(&[(node, 20)]),
            &[],
            &cells,
            &instances,
            &no_ttls(),
            &TIMING,
        );
        assert_eq!(plan.release_cells, vec![a]);
    }

    #[test]
    fn doomed_but_not_yet_due_parent_still_gets_the_note() {
        // Same silent node, child due (short deadline) before its parent
        // (default): the note goes to the parent's db mailbox anyway; if the
        // parent is truly dead it becomes a harmless dead letter.
        let node = rt(1);
        let (p, c) = (sri("p"), sri("c"));
        let cells = vec![entry(p, node), entry(c, node)];
        let instances = vec![
            instance(p, None, false),
            with_deadline(instance(c, Some(p), false), 20_000),
        ];

        let plan = plan_hygiene(
            &silent(&[(node, 25)]),
            &[],
            &cells,
            &instances,
            &no_ttls(),
            &TIMING,
        );
        assert_eq!(plan.release_cells, vec![c]);
        assert_eq!(plan.notes.len(), 1);
        assert_eq!(plan.notes[0].parent, p);
    }

    #[test]
    fn node_declared_ttl_raises_the_default_deadline() {
        // A slow-renewing node declared ttl 90s: its cells' default deadline
        // is 90+15, not the cluster 45+15.
        let node = rt(1);
        let a = sri("a");
        let cells = vec![entry(a, node)];
        let instances = vec![instance(a, None, false)];
        let ttls = HashMap::from([(node, Duration::from_secs(90))]);

        let plan = plan_hygiene(
            &silent(&[(node, 61)]),
            &[],
            &cells,
            &instances,
            &ttls,
            &TIMING,
        );
        assert!(plan.release_cells.is_empty());

        let plan = plan_hygiene(
            &silent(&[(node, 106)]),
            &[],
            &cells,
            &instances,
            &ttls,
            &TIMING,
        );
        assert_eq!(plan.release_cells, vec![a]);
    }

    // ---- root restart planning ----

    use sorg_common::{CellConfig, RestartPolicy, RestartType};

    fn g(n: u64) -> Gen {
        Gen::from_parts(n, 1)
    }

    fn root_spec(name: &str, restart_type: RestartType, max: u32) -> CellDeployment {
        CellDeployment::new(sri(name), CellConfig::Wasm { class: "c".into() }).with_restart(
            RestartPolicy {
                restart_type,
                max_restarts: max,
                window_ms: 60_000,
                delay_ms: 1_000,
            },
        )
    }

    fn entry_gen(s: Sri, node: RuntimeId, gen_id: Gen) -> PlacementEntry {
        PlacementEntry {
            sri: s,
            kind: PlacementKind::Wasm {
                runtime: ExecRuntimeInfo::new(node, None, ExecutionCapabilities::default()),
            },
            app: None,
            gen_id,
        }
    }

    fn death(name: &str, gen_id: Gen, reason: LostReason) -> RootDeath {
        RootDeath {
            sri: sri(name),
            gen_id,
            reason,
        }
    }

    fn plan_of(specs: &[CellDeployment], deaths: &[RootDeath]) -> RestartPlan {
        let mut budget = RestartBudget::new();
        let mut sweep = RestartSweep::default();
        plan_root_restarts(
            specs,
            deaths,
            &[],
            &[],
            &mut budget,
            &mut sweep,
            Instant::now(),
        )
    }

    #[test]
    fn always_crash_restarts_when_rows_gone() {
        let plan = plan_of(
            &[root_spec("r", RestartType::Always, 5)],
            &[death("r", g(1), LostReason::Crashed)],
        );
        assert_eq!(plan.restart, vec![sri("r")]);
        assert!(plan.drop_specs.is_empty());
        assert!(plan.clear_deaths.is_empty());
    }

    #[test]
    fn on_error_clean_stop_is_terminal() {
        let plan = plan_of(
            &[root_spec("r", RestartType::OnError, 5)],
            &[death("r", g(1), LostReason::Stopped { code: Some(0) })],
        );
        assert!(plan.restart.is_empty());
        assert_eq!(plan.drop_specs, vec![sri("r")]);
        assert_eq!(plan.clear_deaths, vec![sri("r")]);
    }

    #[test]
    fn on_error_nonzero_stop_restarts() {
        let plan = plan_of(
            &[root_spec("r", RestartType::OnError, 5)],
            &[death("r", g(1), LostReason::Stopped { code: Some(2) })],
        );
        assert_eq!(plan.restart, vec![sri("r")]);
    }

    #[test]
    fn terminate_is_terminal_even_for_always() {
        let plan = plan_of(
            &[root_spec("r", RestartType::Always, 5)],
            &[death("r", g(1), LostReason::Terminated)],
        );
        assert!(plan.restart.is_empty());
        assert_eq!(plan.drop_specs, vec![sri("r")]);
    }

    #[test]
    fn stale_death_cleared_when_newer_generation_present() {
        let mut budget = RestartBudget::new();
        let mut sweep = RestartSweep::default();
        // A newer generation holds the placement row: the root is already back.
        let plan = plan_root_restarts(
            &[root_spec("r", RestartType::Always, 5)],
            &[death("r", g(1), LostReason::Crashed)],
            &[entry_gen(sri("r"), rt(1), g(2))],
            &[],
            &mut budget,
            &mut sweep,
            Instant::now(),
        );
        assert!(plan.restart.is_empty());
        assert!(plan.drop_specs.is_empty());
        assert_eq!(plan.clear_deaths, vec![sri("r")]);
    }

    #[test]
    fn defers_while_corpse_placement_present() {
        let mut budget = RestartBudget::new();
        let mut sweep = RestartSweep::default();
        // Same-generation placement row still present: the corpse is not yet
        // cleaned up, so wait rather than redeploy onto a claimed SRI.
        let plan = plan_root_restarts(
            &[root_spec("r", RestartType::Always, 5)],
            &[death("r", g(2), LostReason::Crashed)],
            &[entry_gen(sri("r"), rt(1), g(2))],
            &[],
            &mut budget,
            &mut sweep,
            Instant::now(),
        );
        assert_eq!(plan, RestartPlan::default());
    }

    #[test]
    fn defers_while_instance_row_lingers() {
        let mut budget = RestartBudget::new();
        let mut sweep = RestartSweep::default();
        let plan = plan_root_restarts(
            &[root_spec("r", RestartType::Always, 5)],
            &[death("r", g(1), LostReason::Crashed)],
            &[],
            &[instance(sri("r"), None, false)],
            &mut budget,
            &mut sweep,
            Instant::now(),
        );
        assert_eq!(plan, RestartPlan::default());
    }

    #[test]
    fn unmatched_death_is_cleared() {
        let plan = plan_of(&[], &[death("r", g(1), LostReason::Crashed)]);
        assert_eq!(plan.clear_deaths, vec![sri("r")]);
        assert!(plan.restart.is_empty());
        assert!(plan.drop_specs.is_empty());
    }

    #[test]
    fn budget_exhaustion_gives_up() {
        let specs = [root_spec("r", RestartType::Always, 1)];
        let mut budget = RestartBudget::new();
        let mut sweep = RestartSweep::default();
        let now = Instant::now();

        let p1 = plan_root_restarts(
            &specs,
            &[death("r", g(1), LostReason::Crashed)],
            &[],
            &[],
            &mut budget,
            &mut sweep,
            now,
        );
        assert_eq!(p1.restart, vec![sri("r")]);

        // A second crash with the one-restart budget spent: give up.
        let p2 = plan_root_restarts(
            &specs,
            &[death("r", g(2), LostReason::Crashed)],
            &[],
            &[],
            &mut budget,
            &mut sweep,
            now + Duration::from_secs(1),
        );
        assert!(p2.restart.is_empty());
        assert_eq!(p2.drop_specs, vec![sri("r")]);
        assert_eq!(p2.clear_deaths, vec![sri("r")]);
    }

    #[test]
    fn orphan_spec_dropped_after_two_passes() {
        let specs = [root_spec("r", RestartType::Always, 5)];
        let mut budget = RestartBudget::new();
        let mut sweep = RestartSweep::default();
        let now = Instant::now();

        // Pass 1: the spec's root is gone with no death signal, but the sweep
        // waits one more pass before treating it as a deliberate removal.
        let p1 = plan_root_restarts(&specs, &[], &[], &[], &mut budget, &mut sweep, now);
        assert!(p1.drop_specs.is_empty());

        // Pass 2: confirmed orphan.
        let p2 = plan_root_restarts(&specs, &[], &[], &[], &mut budget, &mut sweep, now);
        assert_eq!(p2.drop_specs, vec![sri("r")]);
    }

    #[test]
    fn restart_is_held_off_until_delay_elapses() {
        let mut spec = root_spec("r", RestartType::Always, 5);
        spec.restart.delay_ms = 10_000; // 10s fixed delay between attempts
        let specs = [spec];
        let mut budget = RestartBudget::new();
        let mut sweep = RestartSweep::default();
        let now = Instant::now();

        // First death: no prior attempt, restart immediately.
        let p1 = plan_root_restarts(
            &specs,
            &[death("r", g(1), LostReason::Crashed)],
            &[],
            &[],
            &mut budget,
            &mut sweep,
            now,
        );
        assert_eq!(p1.restart, vec![sri("r")]);

        // Second death 4s later: inside the 10s delay, so defer (not a give-up).
        let p2 = plan_root_restarts(
            &specs,
            &[death("r", g(2), LostReason::Crashed)],
            &[],
            &[],
            &mut budget,
            &mut sweep,
            now + Duration::from_secs(4),
        );
        assert!(p2.restart.is_empty());
        assert!(p2.drop_specs.is_empty());

        // Past the delay: restart again.
        let p3 = plan_root_restarts(
            &specs,
            &[death("r", g(2), LostReason::Crashed)],
            &[],
            &[],
            &mut budget,
            &mut sweep,
            now + Duration::from_secs(11),
        );
        assert_eq!(p3.restart, vec![sri("r")]);
    }

    #[test]
    fn live_root_with_spec_is_left_alone() {
        let specs = [root_spec("r", RestartType::Always, 5)];
        let placements = [entry_gen(sri("r"), rt(1), g(1))];
        let mut budget = RestartBudget::new();
        let mut sweep = RestartSweep::default();
        let now = Instant::now();

        plan_root_restarts(&specs, &[], &placements, &[], &mut budget, &mut sweep, now);
        let p2 = plan_root_restarts(&specs, &[], &placements, &[], &mut budget, &mut sweep, now);
        assert!(p2.drop_specs.is_empty());
        assert!(p2.restart.is_empty());
    }
}

#[cfg(test)]
mod lease_missing_tests {
    use std::collections::HashSet;

    use super::*;

    fn rt(n: u8) -> RuntimeId {
        zenoh_protocol::core::ZenohIdProto::try_from(&[n; 8][..])
            .unwrap()
            .into()
    }

    #[test]
    fn placed_hosts_without_a_lease_row_are_flagged_once() {
        let placed = [rt(1), rt(2), rt(2)];
        let leased: HashSet<_> = [rt(1)].into();
        // rt(1) leases (the tracker owns it); rt(2)'s lease row is gone —
        // flagged once despite hosting two cells.
        assert_eq!(lease_missing(&placed, &leased), vec![rt(2)]);
    }

    #[test]
    fn empty_when_every_placed_host_leases() {
        let placed = [rt(1)];
        let leased: HashSet<_> = [rt(1)].into();
        assert!(lease_missing(&placed, &leased).is_empty());
    }
}
