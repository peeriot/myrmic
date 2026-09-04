//! Previous-incarnation sweep. When an exec restarts it comes back with an
//! empty hosting map while the datalayer still holds the placement rows of
//! the cells it ran before. Those rows are corpses: nothing hosts them, yet
//! `claim_placement` treats them as live and rejects a redeploy of the
//! same SRI with `DuplicateSri`. The sweep releases them.
//!
//! It runs in two places over the same [`select_remnants`] selection: once at
//! boot before the exec advertises as available ([`sweep_previous_incarnation`],
//! so a redeploy cannot race the first verify pass), and again each verify pass
//! (`Runtime::startup_sweep`) until it completes cleanly.

use std::collections::HashSet;

use cell_protocol::{PlacementEntry, PlacementKind, RuntimeId, Sri};
use sorg_common::{
    CellLost, LostReason, emit_cell_lost, instance_registry, list_placements, remove_placement,
};
use tracing::{debug, warn};
use zenoh::Session;

/// The placement rows that name `my_exec` but are not in `hosted` — cells a
/// previous incarnation of this exec left behind. A freshly booted exec hosts
/// nothing, so every row naming it is a remnant.
pub(crate) fn select_remnants(
    cells: &[PlacementEntry],
    my_exec: RuntimeId,
    hosted: &HashSet<Sri>,
) -> Vec<Sri> {
    cells
        .iter()
        .filter(|c| {
            matches!(&c.kind, PlacementKind::Wasm { runtime } if runtime.id() == my_exec)
                && !hosted.contains(&c.sri)
        })
        .map(|c| c.sri)
        .collect()
}

/// Releases this exec's previous-incarnation remnants before it registers as
/// available, so a redeploy of the same SRI cannot race the first verify-pass
/// sweep and hit `DuplicateSri`. Best-effort: any row left behind here is
/// retried by the event loop's periodic `startup_sweep`.
pub(crate) async fn sweep_previous_incarnation(session: &Session, my_exec: RuntimeId) {
    let cells = match list_placements(session).await {
        Ok(cells) => cells,
        Err(err) => {
            debug!("startup sweep: cell scan failed: {err}");
            return;
        }
    };
    let remnants = select_remnants(&cells, my_exec, &HashSet::new());
    if remnants.is_empty() {
        return;
    }
    warn!(
        count = remnants.len(),
        "startup sweep: releasing cells of a previous incarnation"
    );
    for sri in remnants {
        // Notify the parent (its child died with the old process) before the
        // rows go — once they are gone the note can no longer be re-derived, so
        // a failed emit leaves the rows for the verify-pass sweep to retry.
        match instance_registry::get_instance(session, &sri).await {
            Ok(Some(instance)) => {
                if !instance.lineage.detached
                    && let Some(parent) = instance.lineage.parent
                {
                    let note = CellLost {
                        cell: sri,
                        local_name: instance.lineage.local_name.clone(),
                        reason: LostReason::Crashed,
                    };
                    if let Err(err) = emit_cell_lost(session, &parent, note).await {
                        warn!("startup sweep: cell_lost to '{parent}' failed: {err}");
                        continue;
                    }
                }
            }
            Ok(None) => {}
            Err(err) => {
                warn!("startup sweep: instance read for '{sri}' failed: {err}");
                continue;
            }
        }
        if let Err(err) = remove_placement(session, &sri).await {
            warn!("startup sweep: releasing cell row '{sri}' failed: {err}");
            continue;
        }
        // Tolerant erase: undeploy or a concurrent pass may have already
        // removed the instance row, and an absent row means the work is done.
        if let Err(err) = instance_registry::erase_instance_if_present(session, &sri).await {
            debug!("startup sweep: erasing instance row '{sri}': {err}");
        }
    }
}
