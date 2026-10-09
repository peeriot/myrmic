use std::collections::{BTreeMap, HashMap};

use cell_protocol::{ArtifactPlatform, BlobHash, ClassInfo};
use sorg_common::{ArtifactKind, RejectionReason};

use super::super::{CellMappings, preprocess};
use super::{
    class_esp32c6_only, class_id, class_wasm_only, context, esp32c6_rt, linux_rt, rt_id, wasm_cell,
};

#[test]
fn linux_runtime_without_wasm_artifact_is_rejected() {
    let id = rt_id("e689604085684e3e8469c5536703ec14");
    let ctx = context(
        vec![linux_rt(id)],
        HashMap::from([(class_id("cls"), class_esp32c6_only())]),
    );
    let cell = wasm_cell("cls");

    let result = preprocess(std::slice::from_ref(&cell), &ctx);

    let CellMappings::Infeasible(infeasible) = result else {
        panic!("expected CellMappings::Infeasible");
    };
    assert_eq!(
        RejectionReason::MissingArtifact(ArtifactKind::Wasm),
        infeasible[0].rejections[0].reason,
        "linux runtime should be rejected for an AOT-only class"
    );
}

#[test]
fn esp32c6_runtime_without_aot_artifact_is_rejected() {
    let id = rt_id("e689604085684e3e8469c5536703ec14");
    let ctx = context(
        vec![esp32c6_rt(id)],
        HashMap::from([(class_id("cls"), class_wasm_only())]),
    );
    let cell = wasm_cell("cls");

    let result = preprocess(std::slice::from_ref(&cell), &ctx);

    let CellMappings::Infeasible(infeasible) = result else {
        panic!("expected CellMappings::Infeasible");
    };
    assert_eq!(
        RejectionReason::MissingArtifact(ArtifactKind::Aot {
            target: ArtifactPlatform::Riscv32imac,
        }),
        infeasible[0].rejections[0].reason,
        "esp32c6 runtime should be rejected when AOT artifact is absent"
    );
}

#[test]
fn esp32c6_runtime_takes_a_wasm_class_compiled_for_it() {
    let id = rt_id("e689604085684e3e8469c5536703ec14");
    let ctx = context(
        vec![esp32c6_rt(id)],
        HashMap::from([(
            class_id("cls"),
            ClassInfo::Wasm {
                aot: BTreeMap::from([(ArtifactPlatform::Riscv32imac, BlobHash::of(b"aot"))]),
                spawns: vec![],
            },
        )]),
    );
    let cell = wasm_cell("cls");

    let result = preprocess(std::slice::from_ref(&cell), &ctx);

    assert!(
        matches!(result, CellMappings::Trivial(_)),
        "esp32c6 runtime should take a wasm class with an AOT for its platform"
    );
}

/// Defence in depth: `artifact_rejection` still has a `None` branch, because
/// the type allows one. `decide_cell_placement` short-circuits an absent class
/// into `UnknownClass` long before this, so the branch is reachable only by
/// calling `preprocess` directly, as this test does.
#[test]
fn class_absent_from_the_read_still_rejects_a_runtime_in_preprocess() {
    let id = rt_id("e689604085684e3e8469c5536703ec14");
    let ctx = context(vec![linux_rt(id)], HashMap::new());
    let cell = wasm_cell("cls");

    let result = preprocess(std::slice::from_ref(&cell), &ctx);

    let CellMappings::Infeasible(infeasible) = result else {
        panic!("expected CellMappings::Infeasible");
    };
    assert_eq!(
        RejectionReason::MissingArtifact(ArtifactKind::Wasm),
        infeasible[0].rejections[0].reason,
        "linux runtime should be rejected when the class is not in the registry at all"
    );
}
