use std::collections::HashMap;

use cell_protocol::Sri;
use cell_protocol::{
    BlobHash, CapabilityTag, ClassInfo, ExecRuntimeInfo, ExecutionCapabilities, RuntimeId,
};
use sorg_common::{CellConfig, CellDeployment, RejectionReason, RequirementTags};

use super::super::super::PlacementContext;
use super::super::{CellMappings, preprocess};
use super::rt_id;

fn linux_rt_with_tags(id: RuntimeId, tags: Vec<&str>) -> ExecRuntimeInfo {
    let caps = tags.into_iter().map(CapabilityTag::new).collect::<Vec<_>>();
    // "linux" is needed for the artifact check to pass; prepend it.
    let mut all = vec![CapabilityTag::new("linux")];
    all.extend(caps);
    ExecRuntimeInfo::new(id, None, ExecutionCapabilities::new(all))
}

fn wasm_cell_requiring(class: &str, required_tags: Vec<&str>) -> CellDeployment {
    CellDeployment::new(
        Sri::from_target(class).unwrap(),
        CellConfig::Wasm {
            class: class.to_string(),
        },
    )
    .with_tags(RequirementTags::new(required_tags))
}

fn context_linux(id: RuntimeId, rt_tags: Vec<&str>) -> PlacementContext {
    PlacementContext {
        execs: vec![linux_rt_with_tags(id, rt_tags)],
        class_info: HashMap::from([(
            "cls".to_string(),
            ClassInfo {
                name: "cls".to_string(),
                wasm_hash: Some(BlobHash::of(b"")),
                artifacts: vec![],
            },
        )]),
        cells_per_runtime: HashMap::new(),
    }
}

#[test]
fn missing_tag_rejects_runtime() {
    let id = rt_id("e689604085684e3e8469c5536703ec14");
    let ctx = context_linux(id, vec![]);
    let cell = wasm_cell_requiring("cls", vec!["region-a"]);

    let result = preprocess(std::slice::from_ref(&cell), &ctx);

    let CellMappings::Infeasible(infeasible) = result else {
        panic!("expected CellMappings::Infeasible");
    };
    let RejectionReason::MissingTags(ref missing) = infeasible[0].rejections[0].reason else {
        panic!("expected RejectionReason::MissingTags");
    };
    assert!(
        missing.contains(&"region-a".to_string()),
        "missing tags should include 'region-a'"
    );
}

#[test]
fn matching_tag_makes_runtime_eligible() {
    let id = rt_id("e689604085684e3e8469c5536703ec14");
    let ctx = context_linux(id, vec!["region-a"]);
    let cell = wasm_cell_requiring("cls", vec!["region-a"]);

    let result = preprocess(std::slice::from_ref(&cell), &ctx);

    assert!(
        matches!(result, CellMappings::Trivial(_)),
        "runtime satisfying the required tag should be eligible"
    );
}

#[test]
fn runtime_missing_one_of_two_required_tags_is_rejected() {
    let id = rt_id("e689604085684e3e8469c5536703ec14");
    let ctx = context_linux(id, vec!["region-a"]);
    let cell = wasm_cell_requiring("cls", vec!["region-a", "gpu"]);

    let result = preprocess(std::slice::from_ref(&cell), &ctx);

    let CellMappings::Infeasible(infeasible) = result else {
        panic!("expected CellMappings::Infeasible");
    };
    let RejectionReason::MissingTags(ref missing) = infeasible[0].rejections[0].reason else {
        panic!("expected RejectionReason::MissingTags");
    };
    assert!(
        missing.contains(&"gpu".to_string()),
        "missing tags should include 'gpu'"
    );
    assert!(
        !missing.contains(&"region-a".to_string()),
        "present tag 'region-a' should not appear in missing list"
    );
}

#[test]
fn runtime_with_extra_tags_is_eligible() {
    let id = rt_id("e689604085684e3e8469c5536703ec14");
    let ctx = context_linux(id, vec!["region-a", "gpu"]);
    let cell = wasm_cell_requiring("cls", vec!["region-a"]);

    let result = preprocess(std::slice::from_ref(&cell), &ctx);

    assert!(
        matches!(result, CellMappings::Trivial(_)),
        "runtime with a superset of required tags should be eligible"
    );
}
