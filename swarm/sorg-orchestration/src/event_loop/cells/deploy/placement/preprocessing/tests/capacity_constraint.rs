use std::collections::HashMap;

use cell_protocol::Sri;
use sorg_common::RejectionReason;

use super::super::super::PlacementContext;
use super::super::{CellMappings, preprocess};
use super::{class_esp32c6_only, class_wasm_only, esp32c6_rt, linux_rt, rt_id, wasm_cell};

#[test]
fn occupied_esp32c6_runtime_is_rejected() {
    let id = rt_id("e689604085684e3e8469c5536703ec14");
    let ctx = PlacementContext {
        execs: vec![esp32c6_rt(id)],
        class_info: HashMap::from([("cls".to_string(), class_esp32c6_only("cls"))]),
        cells_per_runtime: HashMap::from([(id, vec![Sri::from_target("existing-cell").unwrap()])]),
    };
    let cell = wasm_cell("cls");

    let result = preprocess(std::slice::from_ref(&cell), &ctx);

    let CellMappings::Infeasible(infeasible) = result else {
        panic!("expected CellMappings::Infeasible");
    };
    assert_eq!(
        RejectionReason::AtCapacity,
        infeasible[0].rejections[0].reason,
        "occupied esp32c6 runtime should be rejected with AtCapacity"
    );
}

#[test]
fn occupied_linux_runtime_is_not_rejected() {
    let id = rt_id("e689604085684e3e8469c5536703ec14");
    let ctx = PlacementContext {
        execs: vec![linux_rt(id)],
        class_info: HashMap::from([("cls".to_string(), class_wasm_only("cls"))]),
        cells_per_runtime: HashMap::from([(id, vec![Sri::from_target("existing-cell").unwrap()])]),
    };
    let cell = wasm_cell("cls");

    let result = preprocess(std::slice::from_ref(&cell), &ctx);

    assert!(
        matches!(result, CellMappings::Trivial(_)),
        "linux runtime should remain eligible regardless of how many cells it already hosts"
    );
}
