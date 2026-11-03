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

use cell_protocol::{Gen, PlacementEntry, PlacementKind, RuntimeId, Sri};
use sorg_common::{
    LostReason, instance_registry, list_placements, remove_placement, report_cell_death,
};
use tracing::{debug, warn};
use zenoh::Session;

/// The placement rows that name `my_exec` but are not in `hosted` — cells a
/// previous incarnation of this exec left behind, each with the generation
/// its row carries so the release touches nothing newer. A freshly booted
/// exec hosts nothing, so every row naming it is a remnant.
pub(crate) fn select_remnants(
    cells: &[PlacementEntry],
    my_exec: RuntimeId,
    hosted: &HashSet<Sri>,
) -> Vec<(Sri, Gen)> {
    cells
        .iter()
        .filter(|c| {
            matches!(&c.kind, PlacementKind::Wasm { runtime } if runtime.id() == my_exec)
                && !hosted.contains(&c.sri)
        })
        .map(|c| (c.sri, c.gen_id))
        .collect()
}

/// Reports and releases this exec's previous-incarnation remnants before it
/// registers as available, so a redeploy of the same SRI cannot race the
/// first verify-pass sweep and hit `DuplicateSri`. Best-effort: any row left
/// behind here is retried by the event loop's periodic `startup_sweep`.
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
    for (sri, gen_id) in remnants {
        // Report first: a failed report leaves the rows for the verify-pass
        // sweep to retry, and once they are gone the death cannot be derived.
        let instance_gen = match instance_registry::get_instance(session, &sri).await {
            Ok(Some(instance)) => {
                let report = report_cell_death(
                    session,
                    sri,
                    instance.gen_id,
                    instance.lineage.parent,
                    instance.lineage.detached,
                    instance.lineage.local_name.clone(),
                    LostReason::Crashed,
                )
                .await;
                if let Err(err) = report {
                    warn!("startup sweep: death report for '{sri}' failed: {err}");
                    continue;
                }
                Some(instance.gen_id)
            }
            Ok(None) => None,
            Err(err) => {
                warn!("startup sweep: instance read for '{sri}' failed: {err}");
                continue;
            }
        };
        if let Err(err) = remove_placement(session, &sri, gen_id).await {
            warn!("startup sweep: releasing cell row '{sri}' failed: {err}");
            continue;
        }
        // Tolerant removal: undeploy or a concurrent pass may have already
        // removed the instance row, and an absent row means the work is done.
        if let Some(instance_gen) = instance_gen
            && let Err(err) = instance_registry::remove_instance(session, &sri, instance_gen).await
        {
            debug!("startup sweep: removing instance row '{sri}': {err}");
        }
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

    fn wasm_row(s: Sri, node: RuntimeId, gen_id: Gen) -> PlacementEntry {
        PlacementEntry {
            sri: s,
            kind: PlacementKind::Wasm {
                runtime: ExecRuntimeInfo::new(node, None, ExecutionCapabilities::default()),
            },
            app: None,
            gen_id,
        }
    }

    /// Lineage is not on the placement row, so roots and children select alike.
    #[test]
    fn selects_every_unhosted_row_naming_this_exec() {
        let me = rt(1);
        let root = sri("root");
        let child = sri("child");
        let cells = vec![
            wasm_row(root, me, Gen::from_parts(3, 1)),
            wasm_row(child, me, Gen::from_parts(3, 2)),
        ];

        assert_eq!(
            select_remnants(&cells, me, &HashSet::new()),
            vec![
                (root, Gen::from_parts(3, 1)),
                (child, Gen::from_parts(3, 2))
            ]
        );
    }

    #[test]
    fn skips_hosted_foreign_and_non_wasm_rows() {
        let me = rt(1);
        let hosted = sri("hosted");
        let cells = vec![
            wasm_row(hosted, me, Gen::from_parts(1, 1)),
            wasm_row(sri("elsewhere"), rt(2), Gen::from_parts(1, 1)),
            PlacementEntry {
                sri: sri("native"),
                kind: PlacementKind::Native { runtime: me },
                app: None,
                gen_id: Gen::from_parts(1, 1),
            },
        ];

        assert!(select_remnants(&cells, me, &HashSet::from([hosted])).is_empty());
    }
}
