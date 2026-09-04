use std::collections::HashMap;

use super::super::{CellMappings, PlacementOptions, preprocess};
use super::{class_esp32c6_only, class_wasm_only, context, esp32c6_rt, linux_rt, rt_id, wasm_cell};

#[test]
fn single_runtime_yields_trivial() {
    let id = rt_id("e689604085684e3e8469c5536703ec14");
    let ctx = context(
        vec![linux_rt(id)],
        HashMap::from([("cls".to_string(), class_wasm_only("cls"))]),
    );
    let cell = wasm_cell("cls");

    let result = preprocess(std::slice::from_ref(&cell), &ctx);

    let CellMappings::Trivial(mappings) = result else {
        panic!("expected CellMappings::Trivial");
    };
    assert_eq!(1, mappings.len(), "one mapping for one cell");
    assert!(
        matches!(mappings[0].options, PlacementOptions::Trivial(rt) if rt == id),
        "the single eligible runtime should be the trivial choice"
    );
}

#[test]
fn two_runtimes_yields_untrivial() {
    let id1 = rt_id("e689604085684e3e8469c5536703ec14");
    let id2 = rt_id("1dbb066228ea49ab93ddbb0292168d40");
    let ctx = context(
        vec![linux_rt(id1), linux_rt(id2)],
        HashMap::from([("cls".to_string(), class_wasm_only("cls"))]),
    );
    let cell = wasm_cell("cls");

    let result = preprocess(std::slice::from_ref(&cell), &ctx);

    let CellMappings::Untrivial(mappings) = result else {
        panic!("expected CellMappings::Untrivial");
    };
    assert_eq!(1, mappings.len(), "one mapping for one cell");
    let PlacementOptions::Untrivial(ref ids) = mappings[0].options else {
        panic!("expected PlacementOptions::Untrivial");
    };
    assert_eq!(vec![id1, id2], *ids, "both runtimes should be eligible");
}

#[test]
fn all_trivial_batch_yields_trivial() {
    // cell_a has only a wasm artifact → eligible for the linux runtime only
    // cell_b has only an esp32c6 artifact → eligible for the esp32c6 runtime only
    let linux_id = rt_id("e689604085684e3e8469c5536703ec14");
    let esp_id = rt_id("1dbb066228ea49ab93ddbb0292168d40");
    let ctx = context(
        vec![linux_rt(linux_id), esp32c6_rt(esp_id)],
        HashMap::from([
            ("linux-cls".to_string(), class_wasm_only("linux-cls")),
            ("esp-cls".to_string(), class_esp32c6_only("esp-cls")),
        ]),
    );
    let cell_a = wasm_cell("linux-cls");
    let cell_b = wasm_cell("esp-cls");

    let result = preprocess(&[cell_a, cell_b], &ctx);

    assert!(
        matches!(result, CellMappings::Trivial(_)),
        "all cells have exactly one eligible runtime — batch should be Trivial"
    );
}

#[test]
fn any_untrivial_cell_upgrades_batch_to_untrivial() {
    // cell_a is eligible for two linux runtimes → untrivial
    // cell_b is eligible for one esp32c6 runtime only → trivial
    let linux1 = rt_id("e689604085684e3e8469c5536703ec14");
    let linux2 = rt_id("1dbb066228ea49ab93ddbb0292168d40");
    let esp = rt_id("d3d21458b0a24e819fc9081ea3fdb540");
    let ctx = context(
        vec![linux_rt(linux1), linux_rt(linux2), esp32c6_rt(esp)],
        HashMap::from([
            ("linux-cls".to_string(), class_wasm_only("linux-cls")),
            ("esp-cls".to_string(), class_esp32c6_only("esp-cls")),
        ]),
    );
    let cell_a = wasm_cell("linux-cls");
    let cell_b = wasm_cell("esp-cls");

    let result = preprocess(&[cell_a, cell_b], &ctx);

    assert!(
        matches!(result, CellMappings::Untrivial(_)),
        "one untrivial cell should upgrade the whole batch to Untrivial"
    );
}
