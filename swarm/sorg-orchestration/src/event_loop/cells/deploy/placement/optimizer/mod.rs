use std::collections::{HashMap, HashSet};

use cell_protocol::{RuntimeId, Sri};

use super::preprocessing::{CellMapping, PlacementOptions};
use super::triage::CellBinding;

pub(super) enum OptimizationOutcome {
    Bound(Vec<CellBinding>),
    /// No valid assignment exists: more cells require capacity-limited runtimes
    /// than those runtimes can host. Distinct from an internal error.
    Infeasible,
}

/// Assigns each flexible cell to one of its eligible runtimes, spreading cells so
/// the resulting per-runtime count is as even as possible.
///
/// Each cell goes on its least-loaded eligible runtime, which balances load across
/// the fleet rather than consolidating onto the fewest machines. `existing_load`
/// seeds each runtime's count from the cells already deployed, so the spread
/// accounts for prior deploys and not just this batch.
///
/// Capacity-1 runtimes (embedded) hold at most one cell. A cell whose only
/// eligible runtimes are already-full capacity-1 runtimes makes the batch
/// `Infeasible`.
pub(super) fn bind_untrivial_mappings(
    mappings: Vec<CellMapping>,
    embedded_nodes: &HashSet<RuntimeId>,
    existing_load: &HashMap<RuntimeId, usize>,
) -> OptimizationOutcome {
    let mut load = existing_load.clone();
    let mut bindings = Vec::with_capacity(mappings.len());
    let mut flexible: Vec<(Sri, Vec<RuntimeId>)> = Vec::new();

    // Single-option cells are forced onto their one runtime, but still consume
    // capacity, so fold them into the load before spreading the rest.
    for mapping in mappings {
        match mapping.options {
            PlacementOptions::Trivial(rt_id) => {
                *load.entry(rt_id).or_insert(0) += 1;
                bindings.push(CellBinding {
                    sri: mapping.sri,
                    rt_id,
                });
            }
            PlacementOptions::Untrivial(rt_ids) => flexible.push((mapping.sri, rt_ids)),
            PlacementOptions::Infeasible(..) => {
                unreachable!("infeasible mappings are rejected before optimization")
            }
        }
    }

    // Place the most-constrained cells first (fewest eligible runtimes) so a cell
    // whose only options are capacity-limited isn't starved by one that had
    // alternatives. The sort is stable, so equally-constrained cells keep their
    // batch order and placement stays deterministic.
    flexible.sort_by_key(|(_, rt_ids)| rt_ids.len());

    for (sri, rt_ids) in flexible {
        let Some(rt_id) = pick_least_loaded(&rt_ids, &load, embedded_nodes) else {
            return OptimizationOutcome::Infeasible;
        };
        *load.entry(rt_id).or_insert(0) += 1;
        bindings.push(CellBinding { sri, rt_id });
    }

    OptimizationOutcome::Bound(bindings)
}

/// Returns the eligible runtime carrying the fewest cells, skipping capacity-1
/// runtimes that are already occupied. Ties go to the earliest runtime in
/// `eligible` (built in a stable order by preprocessing), keeping the choice
/// deterministic. `None` means every eligible runtime is a full capacity-1
/// runtime.
fn pick_least_loaded(
    eligible: &[RuntimeId],
    load: &HashMap<RuntimeId, usize>,
    embedded_nodes: &HashSet<RuntimeId>,
) -> Option<RuntimeId> {
    eligible
        .iter()
        .filter(|rt_id| {
            let current = load.get(*rt_id).copied().unwrap_or(0);
            !(embedded_nodes.contains(*rt_id) && current >= 1)
        })
        .min_by_key(|rt_id| load.get(*rt_id).copied().unwrap_or(0))
        .copied()
}

#[cfg(test)]
mod tests;
