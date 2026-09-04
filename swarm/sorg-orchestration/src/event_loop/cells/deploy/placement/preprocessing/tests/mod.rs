mod happy_path;

mod tags_constraint;

mod artifact_constraint;

mod capacity_constraint;

use std::collections::HashMap;
use std::str::FromStr;

use cell_protocol::{
    ArtifactInfo, ArtifactPlatform, BlobHash, CapabilityTag, ClassInfo, ExecRuntimeInfo,
    ExecutionCapabilities, RuntimeId, Sri,
};
use sorg_common::{CellConfig, CellDeployment, RejectionReason};
use zenoh::config::ZenohId;

use super::super::PlacementContext;
use super::{CellMappings, preprocess};

fn rt_id(hex: &str) -> RuntimeId {
    ZenohId::from_str(hex).unwrap().into()
}

fn linux_rt(id: RuntimeId) -> ExecRuntimeInfo {
    ExecRuntimeInfo::new(
        id,
        None,
        ExecutionCapabilities::new(vec![CapabilityTag::new("linux")]),
    )
}

fn esp32c6_rt(id: RuntimeId) -> ExecRuntimeInfo {
    ExecRuntimeInfo::new(
        id,
        None,
        ExecutionCapabilities::new(vec![CapabilityTag::new("esp32c6")]),
    )
}

fn unknown_rt(id: RuntimeId) -> ExecRuntimeInfo {
    ExecRuntimeInfo::new(id, None, ExecutionCapabilities::default())
}

fn wasm_cell(class: &str) -> CellDeployment {
    CellDeployment::new(
        Sri::from_target(class).unwrap(),
        CellConfig::Wasm {
            class: class.to_string(),
        },
    )
}

fn class_wasm_only(name: &str) -> ClassInfo {
    ClassInfo {
        name: name.to_string(),
        wasm_hash: Some(BlobHash::of(b"")),
        artifacts: vec![],
    }
}

fn class_esp32c6_only(name: &str) -> ClassInfo {
    ClassInfo {
        name: name.to_string(),
        wasm_hash: None,
        artifacts: vec![ArtifactInfo {
            platform: ArtifactPlatform::Riscv32imac,
            aot_hash: BlobHash::of(b"aot"),
            meta_hash: BlobHash::of(b"meta"),
        }],
    }
}

fn context(
    execs: Vec<ExecRuntimeInfo>,
    class_info: HashMap<String, ClassInfo>,
) -> PlacementContext {
    PlacementContext {
        execs,
        class_info,
        cells_per_runtime: HashMap::new(),
    }
}

#[test]
fn unknown_runtime_is_rejected() {
    let id = rt_id("e689604085684e3e8469c5536703ec14");
    let ctx = context(
        vec![unknown_rt(id)],
        HashMap::from([("cls".to_string(), class_wasm_only("cls"))]),
    );
    let cell = wasm_cell("cls");

    let result = preprocess(std::slice::from_ref(&cell), &ctx);

    let CellMappings::Infeasible(infeasible) = result else {
        panic!("expected CellMappings::Infeasible for an unknown runtime");
    };
    assert_eq!(1, infeasible.len(), "one cell should be infeasible");
    assert_eq!(
        1,
        infeasible[0].rejections.len(),
        "one rejection for the one runtime"
    );
    assert_eq!(
        RejectionReason::UnsupportedRuntime,
        infeasible[0].rejections[0].reason,
        "unknown runtime should be rejected with UnsupportedRuntime"
    );
}
