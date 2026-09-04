use std::collections::{HashMap, HashSet};

use cell_protocol::{RuntimeId, Sri};
use sorg_common::CellInfeasibility;

use super::optimizer::OptimizationOutcome;
use super::preprocessing::{CellMapping, CellMappings, PlacementOptions};

#[derive(Debug, PartialEq)]
pub(super) struct CellBinding {
    pub(super) sri: Sri,
    pub(super) rt_id: RuntimeId,
}

pub(super) enum TriageOutcome {
    Bound(Vec<CellBinding>),
    Infeasible(Vec<CellInfeasibility>),
    /// All cells have eligible runtimes but no valid joint assignment exists —
    /// the cells conflict over shared capacity-1 resources.
    PlacementConflicts,
}

pub(super) fn triage(
    cell_mappings: CellMappings,
    embedded_nodes: &HashSet<RuntimeId>,
    existing_load: &HashMap<RuntimeId, usize>,
) -> TriageOutcome {
    match cell_mappings {
        CellMappings::Infeasible(infeasible) => TriageOutcome::Infeasible(infeasible),
        CellMappings::Trivial(mappings) => bind_trivial(mappings, embedded_nodes),
        CellMappings::Untrivial(mappings) => {
            bind_untrivial(mappings, embedded_nodes, existing_load)
        }
    }
}

fn bind_trivial(mappings: Vec<CellMapping>, embedded_nodes: &HashSet<RuntimeId>) -> TriageOutcome {
    let bindings: Vec<CellBinding> = mappings
        .into_iter()
        .map(|m| {
            let PlacementOptions::Trivial(rt_id) = m.options else {
                unreachable!("all mappings are trivial");
            };
            CellBinding { sri: m.sri, rt_id }
        })
        .collect();

    // Detect capacity violations: a capacity-1 runtime assigned to more than
    // one cell in this batch cannot host all of them.
    let mut counts: HashMap<RuntimeId, usize> = HashMap::new();
    for b in &bindings {
        if embedded_nodes.contains(&b.rt_id) {
            *counts.entry(b.rt_id).or_insert(0) += 1;
        }
    }
    if counts.values().any(|&c| c > 1) {
        return TriageOutcome::PlacementConflicts;
    }

    TriageOutcome::Bound(bindings)
}

fn bind_untrivial(
    mappings: Vec<CellMapping>,
    embedded_nodes: &HashSet<RuntimeId>,
    existing_load: &HashMap<RuntimeId, usize>,
) -> TriageOutcome {
    match super::optimizer::bind_untrivial_mappings(mappings, embedded_nodes, existing_load) {
        OptimizationOutcome::Bound(bindings) => TriageOutcome::Bound(bindings),
        OptimizationOutcome::Infeasible => TriageOutcome::PlacementConflicts,
    }
}
