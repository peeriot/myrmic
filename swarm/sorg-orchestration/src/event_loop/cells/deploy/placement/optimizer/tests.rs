use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use cell_protocol::{RuntimeId, Sri};
use zenoh::config::ZenohId;

use crate::event_loop::cells::deploy::placement::preprocessing::{CellMapping, PlacementOptions};
use crate::event_loop::cells::deploy::placement::triage::CellBinding;

use super::{OptimizationOutcome, bind_untrivial_mappings};

fn rt(hex: &str) -> RuntimeId {
    ZenohId::from_str(hex).unwrap().into()
}

fn sri(name: &str) -> Sri {
    Sri::from_target(name).unwrap()
}

fn untrivial(name: &str, options: Vec<RuntimeId>) -> CellMapping {
    CellMapping {
        sri: sri(name),
        options: PlacementOptions::Untrivial(options),
    }
}

fn bound(outcome: OptimizationOutcome) -> Vec<CellBinding> {
    match outcome {
        OptimizationOutcome::Bound(bindings) => bindings,
        OptimizationOutcome::Infeasible => panic!("expected a feasible placement"),
    }
}

#[test]
fn spreads_flexible_cells_across_runtimes() {
    let rt1 = rt("e689604085684e3e8469c5536703ec14");
    let rt2 = rt("1dbb066228ea49ab93ddbb0292168d40");
    let rt3 = rt("d3d21458b0a24e819fc9081ea3fdb540");

    let mappings = vec![
        untrivial("cell_a", vec![rt1, rt2, rt3]),
        untrivial("cell_b", vec![rt1, rt2, rt3]),
        untrivial("cell_c", vec![rt1, rt2, rt3]),
    ];

    let bindings = bound(bind_untrivial_mappings(
        mappings,
        &HashSet::new(),
        &HashMap::new(),
    ));

    let used: HashSet<RuntimeId> = bindings.iter().map(|b| b.rt_id).collect();
    assert_eq!(
        used.len(),
        3,
        "each cell should land on a distinct runtime instead of consolidating"
    );
}

#[test]
fn prefers_least_loaded_runtime() {
    let rt1 = rt("e689604085684e3e8469c5536703ec14");
    let rt2 = rt("1dbb066228ea49ab93ddbb0292168d40");

    // rt1 already hosts five cells, so the new cell should go to the emptier rt2.
    let existing_load = HashMap::from([(rt1, 5)]);
    let mappings = vec![untrivial("cell_a", vec![rt1, rt2])];

    let bindings = bound(bind_untrivial_mappings(
        mappings,
        &HashSet::new(),
        &existing_load,
    ));

    assert_eq!(
        bindings,
        vec![CellBinding {
            sri: sri("cell_a"),
            rt_id: rt2,
        }]
    );
}

#[test]
fn fixed_placements_count_toward_load() {
    let rt1 = rt("e689604085684e3e8469c5536703ec14");
    let rt2 = rt("1dbb066228ea49ab93ddbb0292168d40");

    // A forced placement occupies rt1, so the flexible cell should spread to rt2.
    let mappings = vec![
        CellMapping {
            sri: sri("cell_fixed"),
            options: PlacementOptions::Trivial(rt1),
        },
        untrivial("cell_flex", vec![rt1, rt2]),
    ];

    let bindings = bound(bind_untrivial_mappings(
        mappings,
        &HashSet::new(),
        &HashMap::new(),
    ));

    let flex = bindings
        .iter()
        .find(|b| b.sri == sri("cell_flex"))
        .expect("flexible cell should be bound");
    assert_eq!(
        flex.rt_id, rt2,
        "flexible cell should avoid the runtime already holding the fixed cell"
    );
}

#[test]
fn capacity_one_runtimes_hold_one_cell_each() {
    let rt1 = rt("e689604085684e3e8469c5536703ec14"); // capacity-1
    let rt2 = rt("1dbb066228ea49ab93ddbb0292168d40"); // unlimited

    let mappings = vec![
        untrivial("cell_a", vec![rt1, rt2]),
        untrivial("cell_b", vec![rt1, rt2]),
    ];
    let capacity_one = HashSet::from([rt1]);

    let bindings = bound(bind_untrivial_mappings(
        mappings,
        &capacity_one,
        &HashMap::new(),
    ));

    let used: HashSet<RuntimeId> = bindings.iter().map(|b| b.rt_id).collect();
    assert_eq!(
        used.len(),
        2,
        "two cells must not share the capacity-1 runtime"
    );
}

#[test]
fn over_subscribed_capacity_one_is_infeasible() {
    let rt1 = rt("e689604085684e3e8469c5536703ec14");
    let rt2 = rt("1dbb066228ea49ab93ddbb0292168d40");

    // Three cells whose only options are two capacity-1 runtimes: no valid fit.
    let mappings = vec![
        untrivial("cell_a", vec![rt1, rt2]),
        untrivial("cell_b", vec![rt1, rt2]),
        untrivial("cell_c", vec![rt1, rt2]),
    ];
    let capacity_one = HashSet::from([rt1, rt2]);

    let outcome = bind_untrivial_mappings(mappings, &capacity_one, &HashMap::new());
    assert!(matches!(outcome, OptimizationOutcome::Infeasible));
}
