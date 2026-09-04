//! The supersede rule at the spawn duplicate gates (spec §4), plus the pure
//! tree walks used by cascade and terminate scoping.

use std::collections::HashMap;

use cell_protocol::{CellInstance, Gen, Sri};

/// What the gate knows about an existing instance row for the requested SRI.
#[derive(Debug, Clone, Copy)]
pub struct ExistingInstance {
    pub detached: bool,
    pub parent_gen_id: Option<Gen>,
}

/// Liveness view of the existing instance's placement row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseView {
    Live,
    Expired,
}

#[derive(Debug, Clone, Copy)]
pub struct ExistingPlacement {
    pub lease: LeaseView,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    Admit,
    AlreadyExists,
    /// The existing rows are a corpse or belong to a dead parent
    /// incarnation: release them (ordered, in their own committed
    /// transaction) and proceed with the deploy.
    Supersede,
}

/// Spec §4. `caller_gen` is the spawning parent's incarnation; external
/// deploys have none and never supersede a live instance.
pub fn evaluate_spawn_gate(
    existing: Option<&ExistingInstance>,
    placement: Option<&ExistingPlacement>,
    caller_gen: Option<Gen>,
) -> GateDecision {
    let Some(existing) = existing else {
        return GateDecision::Admit;
    };

    let placement_live = match placement {
        Some(p) => p.lease != LeaseView::Expired,
        None => false,
    };
    if !placement_live {
        return GateDecision::Supersede;
    }

    if existing.detached {
        return GateDecision::AlreadyExists;
    }

    match (existing.parent_gen_id, caller_gen) {
        // Live child of THIS parent incarnation: idempotent respawn.
        (Some(row_parent), Some(caller)) if row_parent == caller => GateDecision::AlreadyExists,
        // Live child of a DEAD parent incarnation: doomed whether or not it
        // has fenced yet.
        (Some(_), Some(_)) => GateDecision::Supersede,
        // Unanchored rows (external deploys) : conservative.
        _ => GateDecision::AlreadyExists,
    }
}

/// True when `target` is `ancestor` itself or lies below it in the spawn
/// tree. Detached edges do NOT break authority (spec §8). Capped walk for
/// cycle safety on corrupt registries.
pub fn is_self_or_descendant(
    instances: &HashMap<Sri, CellInstance>,
    ancestor: &Sri,
    target: &Sri,
) -> bool {
    let mut current = *target;
    for _ in 0..64 {
        if current == *ancestor {
            return true;
        }
        match instances.get(&current).and_then(|i| i.lineage.parent) {
            Some(parent) => current = parent,
            None => return false,
        }
    }
    false
}

/// BFS below `root` through non-detached edges; `root` itself is NOT
/// included. Order is parents-before-children, deterministic given input
/// order. Orphan rows pointing at missing parents are simply never reached.
pub fn collect_subtree(instances: &[CellInstance], root: &Sri) -> Vec<Sri> {
    let mut children_of: HashMap<Sri, Vec<&CellInstance>> = HashMap::new();
    for info in instances {
        if let Some(parent) = info.lineage.parent {
            children_of.entry(parent).or_default().push(info);
        }
    }

    let mut out = Vec::new();
    let mut frontier = vec![*root];
    while let Some(current) = frontier.pop() {
        let Some(kids) = children_of.get(&current) else {
            continue;
        };
        for kid in kids {
            if kid.lineage.detached {
                continue;
            }
            out.push(kid.sri);
            frontier.push(kid.sri);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sri(name: &str) -> Sri {
        cell_protocol::sri_of_path(name).unwrap().into()
    }

    // Deliberately Option: call sites pass it straight into Option<Gen> params.
    #[allow(clippy::unnecessary_wraps)]
    fn id(n: u128) -> Option<Gen> {
        Some(Gen::from_parts(0, n))
    }

    fn existing(detached: bool, parent_instance: Option<Gen>) -> ExistingInstance {
        ExistingInstance {
            detached,
            parent_gen_id: parent_instance,
        }
    }

    fn placed(lease: LeaseView) -> ExistingPlacement {
        ExistingPlacement { lease }
    }

    #[test]
    fn no_row_admits() {
        assert_eq!(evaluate_spawn_gate(None, None, id(1)), GateDecision::Admit);
    }

    #[test]
    fn live_detached_already_exists_regardless_of_caller() {
        let e = existing(true, id(99));
        let p = placed(LeaseView::Live);
        assert_eq!(
            evaluate_spawn_gate(Some(&e), Some(&p), id(1)),
            GateDecision::AlreadyExists
        );
        assert_eq!(
            evaluate_spawn_gate(Some(&e), Some(&p), None),
            GateDecision::AlreadyExists
        );
    }

    #[test]
    fn live_child_of_this_incarnation_already_exists() {
        let e = existing(false, id(1));
        let p = placed(LeaseView::Live);
        assert_eq!(
            evaluate_spawn_gate(Some(&e), Some(&p), id(1)),
            GateDecision::AlreadyExists
        );
    }

    #[test]
    fn live_child_of_dead_incarnation_supersedes() {
        let e = existing(false, id(1));
        let p = placed(LeaseView::Live);
        assert_eq!(
            evaluate_spawn_gate(Some(&e), Some(&p), id(2)),
            GateDecision::Supersede
        );
    }

    #[test]
    fn corpse_placement_absent_supersedes() {
        let e = existing(false, id(1));
        assert_eq!(
            evaluate_spawn_gate(Some(&e), None, id(1)),
            GateDecision::Supersede
        );
    }

    #[test]
    fn corpse_lease_expired_supersedes_even_for_external() {
        let e = existing(false, id(1));
        let p = placed(LeaseView::Expired);
        assert_eq!(
            evaluate_spawn_gate(Some(&e), Some(&p), None),
            GateDecision::Supersede
        );
    }

    #[test]
    fn external_deploy_never_supersedes_live() {
        let e = existing(false, id(1));
        let p = placed(LeaseView::Live);
        assert_eq!(
            evaluate_spawn_gate(Some(&e), Some(&p), None),
            GateDecision::AlreadyExists
        );
    }

    #[test]
    fn unanchored_row_conservative_when_live() {
        let e = existing(false, None);
        let p = placed(LeaseView::Live);
        assert_eq!(
            evaluate_spawn_gate(Some(&e), Some(&p), id(1)),
            GateDecision::AlreadyExists
        );
    }

    fn info(s: Sri, parent: Option<Sri>, detached: bool) -> CellInstance {
        CellInstance {
            sri: s,
            class_name: "c".into(),
            gen_id: Gen::from_parts(0, 1),
            lineage: cell_protocol::SpawnLineage {
                parent,
                detached,
                ..Default::default()
            },
        }
    }

    #[test]
    fn ancestry_walk_finds_self_and_descendants_through_detached() {
        let (a, b, c) = (sri("a"), sri("b"), sri("c"));
        let map: HashMap<Sri, CellInstance> = [
            (a, info(a, None, false)),
            (b, info(b, Some(a), true)), // detached: authority still holds
            (c, info(c, Some(b), false)),
        ]
        .into();
        assert!(is_self_or_descendant(&map, &a, &a));
        assert!(is_self_or_descendant(&map, &a, &c));
        assert!(!is_self_or_descendant(&map, &b, &a));
    }

    #[test]
    fn ancestry_walk_terminates_on_cycles() {
        let (a, b) = (sri("a"), sri("b"));
        let map: HashMap<Sri, CellInstance> =
            [(a, info(a, Some(b), false)), (b, info(b, Some(a), false))].into();
        assert!(!is_self_or_descendant(&map, &sri("other"), &a));
    }

    #[test]
    fn subtree_excludes_detached_branches_and_their_children() {
        let (r, k1, k2, d, dk) = (sri("r"), sri("k1"), sri("k2"), sri("d"), sri("dk"));
        let instances = vec![
            info(r, None, false),
            info(k1, Some(r), false),
            info(k2, Some(k1), false),
            info(d, Some(r), true),
            info(dk, Some(d), false),
        ];
        let subtree = collect_subtree(&instances, &r);
        assert!(subtree.contains(&k1));
        assert!(subtree.contains(&k2));
        assert!(!subtree.contains(&d));
        assert!(!subtree.contains(&dk));
        assert!(!subtree.contains(&r));
    }

    #[test]
    fn subtree_of_leaf_is_empty() {
        let r = sri("r");
        assert!(collect_subtree(&[info(r, None, false)], &r).is_empty());
    }
}
