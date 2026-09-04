use std::collections::HashMap;

use cell_protocol::{ArtifactPlatform, BlobHash, ClassInfo};
use sorg_common::{ArtifactKind, RejectionReason};

use super::super::{CellMappings, preprocess};
use super::{context, esp32c6_rt, linux_rt, rt_id, wasm_cell};

#[test]
fn linux_runtime_without_wasm_artifact_is_rejected() {
    let id = rt_id("e689604085684e3e8469c5536703ec14");
    let ctx = context(
        vec![linux_rt(id)],
        HashMap::from([(
            "cls".to_string(),
            ClassInfo {
                name: "cls".to_string(),
                wasm_hash: None,
                artifacts: vec![],
            },
        )]),
    );
    let cell = wasm_cell("cls");

    let result = preprocess(std::slice::from_ref(&cell), &ctx);

    let CellMappings::Infeasible(infeasible) = result else {
        panic!("expected CellMappings::Infeasible");
    };
    assert_eq!(
        RejectionReason::MissingArtifact(ArtifactKind::Wasm),
        infeasible[0].rejections[0].reason,
        "linux runtime should be rejected when wasm artifact is absent"
    );
}

#[test]
fn esp32c6_runtime_without_aot_artifact_is_rejected() {
    let id = rt_id("e689604085684e3e8469c5536703ec14");
    let ctx = context(
        vec![esp32c6_rt(id)],
        HashMap::from([(
            "cls".to_string(),
            ClassInfo {
                name: "cls".to_string(),
                wasm_hash: Some(BlobHash::of(b"")),
                artifacts: vec![],
            },
        )]),
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
fn class_not_in_registry_rejects_runtime() {
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
