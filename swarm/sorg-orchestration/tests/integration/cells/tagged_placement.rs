use cell_protocol::PlacementKind;
use claims::{assert_err, assert_ok};
use sorg_common::{
    CellConfig, CellDeployment, DeployRequest, DeploymentError, HttpBridgeApi, RejectionReason,
    RequirementTags, check_tag_requirements,
};
use sorg_tests::{build_and_register_cell_class, swarm_config};

use crate::integration::{spawn_test_app_with_swarm, to_sri};

const APP_NAME: &str = "tag_test_app";
const CELL_SRI: &str = "tagged_cell";

const TAG_GPU: &str = "gpu";
const TAG_CPU: &str = "cpu";
const TAG_SENSOR: &str = "sensor";
const TAG_FPGA: &str = "fpga";

fn wasm_cell(sri: &str) -> CellDeployment {
    CellDeployment::new(
        to_sri(sri),
        CellConfig::Wasm {
            class: "tagged_cell.wasm".to_owned(),
        },
    )
}

fn deploy_request(cells: Vec<CellDeployment>) -> DeployRequest {
    DeployRequest::new(
        cells
            .into_iter()
            .map(|cell| cell.with_app(Some(APP_NAME.to_owned())))
            .collect(),
    )
}

fn find_runtime_matching(
    runtimes: &[cell_protocol::ExecRuntimeInfo],
    tags: &RequirementTags,
) -> cell_protocol::RuntimeId {
    runtimes
        .iter()
        .find(|rt| check_tag_requirements(rt.capabilities(), tags).is_met())
        .unwrap_or_else(|| panic!("no runtime satisfies tags {tags:?}"))
        .id()
}

// Case 1: cell requires [gpu]; R1 has gpu, R2 has cpu → lands on R1.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn single_tag_match() {
    // Arrange — orch+db, two execs with distinct tags
    let swarm = swarm_config!("cells/orch_only.jsonnet");
    let _exec_gpu = swarm_config!("cells/tagged_placement/exec_gpu.jsonnet");
    let _exec_cpu = swarm_config!("cells/tagged_placement/exec_cpu.jsonnet");

    build_and_register_cell_class("../../tests/fixtures/dummy_cell", "tagged_cell", &swarm).await;

    let test_app = spawn_test_app_with_swarm(swarm).await;
    let sorg = sorg_client::Client::new(test_app.session().clone());

    let tags = RequirementTags::new(vec![TAG_GPU]);
    let request = deploy_request(vec![wasm_cell(CELL_SRI).with_tags(tags.clone())]);

    // Act — deploy an app with one gpu-tagged cell
    assert_ok!(sorg.deploy_cells(request).await);

    // Assert — the cell landed on the gpu runtime
    let entry = assert_ok!(sorg.get_placement(&to_sri(CELL_SRI)).await)
        .expect("cell should be registered after deploy");
    let node_id = super::assert_wasm_runtime_id(&entry);
    let runtimes = assert_ok!(sorg.list_exec_runtimes().await);
    let expected = find_runtime_matching(&runtimes, &tags);

    assert_eq!(
        node_id, expected,
        "cell should land on the gpu-tagged runtime, not an arbitrary one"
    );
}

// Case 2: cell requires [gpu, sensor]; only R1 has both → lands on R1.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn multi_tag_and_match() {
    // Arrange — orch+db, one exec with gpu, one with gpu+sensor
    let swarm = swarm_config!("cells/orch_only.jsonnet");
    let _exec_gpu = swarm_config!("cells/tagged_placement/exec_gpu.jsonnet");
    let _exec_gpu_sensor = swarm_config!("cells/tagged_placement/exec_gpu_sensor.jsonnet");

    build_and_register_cell_class("../../tests/fixtures/dummy_cell", "tagged_cell", &swarm).await;

    let test_app = spawn_test_app_with_swarm(swarm).await;
    let sorg = sorg_client::Client::new(test_app.session().clone());

    let tags = RequirementTags::new(vec![TAG_GPU, TAG_SENSOR]);
    let request = deploy_request(vec![wasm_cell(CELL_SRI).with_tags(tags.clone())]);

    // Act — deploy an app with one cell requiring both tags
    assert_ok!(sorg.deploy_cells(request).await);

    // Assert — the cell landed on the dual-tagged runtime
    let entry = assert_ok!(sorg.get_placement(&to_sri(CELL_SRI)).await)
        .expect("cell should be registered after deploy");
    let node_id = super::assert_wasm_runtime_id(&entry);
    let runtimes = assert_ok!(sorg.list_exec_runtimes().await);
    let expected = find_runtime_matching(&runtimes, &tags);

    assert_eq!(
        node_id, expected,
        "cell should land on the runtime with both tags, not the gpu-only one"
    );
}

// Case 3: cell with no tags deploys onto whatever runtime exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn untagged_placeable_anywhere() {
    // Arrange — orch+db, one tagged exec
    let swarm = swarm_config!("cells/orch_only.jsonnet");
    let _exec_gpu = swarm_config!("cells/tagged_placement/exec_gpu.jsonnet");

    build_and_register_cell_class("../../tests/fixtures/dummy_cell", "tagged_cell", &swarm).await;

    let test_app = spawn_test_app_with_swarm(swarm).await;
    let sorg = sorg_client::Client::new(test_app.session().clone());

    let request = deploy_request(vec![wasm_cell(CELL_SRI)]);

    // Act — deploy an untagged cell
    assert_ok!(sorg.deploy_cells(request).await);

    // Assert — the cell is registered (no tag constraint to check)
    let entry = assert_ok!(sorg.get_placement(&to_sri(CELL_SRI)).await)
        .expect("untagged cell should be registered after deploy");
    super::assert_wasm_runtime_id(&entry);
}

// Case 4: two untagged cells, two runtimes → they spread one-per-node.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn untagged_cells_spread_across_nodes() {
    // Arrange — orch+db, two execs
    let swarm = swarm_config!("cells/orch_only.jsonnet");
    let _exec_gpu = swarm_config!("cells/tagged_placement/exec_gpu.jsonnet");
    let _exec_cpu = swarm_config!("cells/tagged_placement/exec_cpu.jsonnet");

    build_and_register_cell_class("../../tests/fixtures/dummy_cell", "tagged_cell", &swarm).await;

    let test_app = spawn_test_app_with_swarm(swarm).await;
    let sorg = sorg_client::Client::new(test_app.session().clone());

    let request = deploy_request(vec![wasm_cell("cell_a"), wasm_cell("cell_b")]);

    // Act — deploy two untagged cells
    assert_ok!(sorg.deploy_cells(request).await);

    // Assert — they spread onto different nodes rather than piling up
    let entry_a = assert_ok!(sorg.get_placement(&to_sri("cell_a")).await)
        .expect("cell_a should be registered");
    let entry_b = assert_ok!(sorg.get_placement(&to_sri("cell_b")).await)
        .expect("cell_b should be registered");
    let node_a = super::assert_wasm_runtime_id(&entry_a);
    let node_b = super::assert_wasm_runtime_id(&entry_b);

    assert_ne!(
        node_a, node_b,
        "two untagged cells should spread across the two nodes, not consolidate"
    );
}

// Case 5: cell X requires [gpu] (forces R1), cell Y untagged → Y spreads to the empty R2.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn untagged_spreads_off_tagged_node() {
    // Arrange — orch+db, two execs with distinct tags
    let swarm = swarm_config!("cells/orch_only.jsonnet");
    let _exec_gpu = swarm_config!("cells/tagged_placement/exec_gpu.jsonnet");
    let _exec_cpu = swarm_config!("cells/tagged_placement/exec_cpu.jsonnet");

    build_and_register_cell_class("../../tests/fixtures/dummy_cell", "tagged_cell", &swarm).await;

    let test_app = spawn_test_app_with_swarm(swarm).await;
    let sorg = sorg_client::Client::new(test_app.session().clone());

    let gpu_tags = RequirementTags::new(vec![TAG_GPU]);
    let request = deploy_request(vec![
        wasm_cell("pinned").with_tags(gpu_tags.clone()),
        wasm_cell("free"),
    ]);

    // Act — deploy one pinned + one free cell
    assert_ok!(sorg.deploy_cells(request).await);

    // Assert — pinned stays on gpu; free spreads to the empty cpu node
    let entry_pinned = assert_ok!(sorg.get_placement(&to_sri("pinned")).await)
        .expect("pinned cell should be registered");
    let entry_free = assert_ok!(sorg.get_placement(&to_sri("free")).await)
        .expect("free cell should be registered");
    let node_pinned = super::assert_wasm_runtime_id(&entry_pinned);
    let node_free = super::assert_wasm_runtime_id(&entry_free);

    let runtimes = assert_ok!(sorg.list_exec_runtimes().await);
    let gpu_rt = find_runtime_matching(&runtimes, &gpu_tags);
    let cpu_rt = find_runtime_matching(&runtimes, &RequirementTags::new(vec![TAG_CPU]));

    assert_eq!(
        node_pinned, gpu_rt,
        "pinned cell should land on the gpu runtime"
    );
    assert_eq!(
        node_free, cpu_rt,
        "free cell should spread to the empty cpu node, not pile onto the occupied gpu node"
    );
}

// Case 6: cell requires [fpga], no runtime has it → deploy fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn unplaceable_tag_absent() {
    // Arrange — orch+db, two execs (gpu + cpu), neither has fpga
    let swarm = swarm_config!("cells/orch_only.jsonnet");
    let _exec_gpu = swarm_config!("cells/tagged_placement/exec_gpu.jsonnet");
    let _exec_cpu = swarm_config!("cells/tagged_placement/exec_cpu.jsonnet");

    build_and_register_cell_class("../../tests/fixtures/dummy_cell", "tagged_cell", &swarm).await;

    let test_app = spawn_test_app_with_swarm(swarm).await;
    let sorg = sorg_client::Client::new(test_app.session().clone());

    let request = deploy_request(vec![
        wasm_cell(CELL_SRI).with_tags(RequirementTags::new(vec![TAG_FPGA])),
    ]);

    // Act — deploy a cell requiring a tag no runtime has
    let err = assert_err!(
        sorg.deploy_cells(request).await,
        "deploy should fail when no runtime has the required tag"
    );

    // Assert — infeasible, naming the cell and the unmet tag; cell not registered
    let DeploymentError::Infeasible(cells) = &err else {
        panic!("expected Infeasible, got: {err:?}");
    };
    let cell = cells
        .iter()
        .find(|c| c.cell == to_sri(CELL_SRI))
        .unwrap_or_else(|| panic!("infeasibility should name cell '{CELL_SRI}', got: {cells:?}"));
    assert!(
        cell.rejections.iter().any(|r| matches!(
            &r.reason,
            RejectionReason::MissingTags(tags) if tags.iter().any(|t| t.as_str() == TAG_FPGA)
        )),
        "expected a MissingTags rejection naming '{TAG_FPGA}', got: {:?}",
        cell.rejections
    );
    assert!(
        assert_ok!(sorg.get_placement(&to_sri(CELL_SRI)).await).is_none(),
        "cell should not be registered after failed deploy"
    );
}

// Case 7: cell requires [gpu, cpu]; R1 has gpu, R2 has cpu, none has both → fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn unplaceable_and_not_satisfiable() {
    // Arrange — orch+db, two execs with one tag each
    let swarm = swarm_config!("cells/orch_only.jsonnet");
    let _exec_gpu = swarm_config!("cells/tagged_placement/exec_gpu.jsonnet");
    let _exec_cpu = swarm_config!("cells/tagged_placement/exec_cpu.jsonnet");

    build_and_register_cell_class("../../tests/fixtures/dummy_cell", "tagged_cell", &swarm).await;

    let test_app = spawn_test_app_with_swarm(swarm).await;
    let sorg = sorg_client::Client::new(test_app.session().clone());

    let request = deploy_request(vec![
        wasm_cell(CELL_SRI).with_tags(RequirementTags::new(vec![TAG_GPU, TAG_CPU])),
    ]);

    // Act — deploy a cell requiring both tags (no single runtime has both)
    let err = assert_err!(
        sorg.deploy_cells(request).await,
        "deploy should fail when no single runtime satisfies all required tags"
    );

    // Assert — infeasible, the cell rejected by every runtime on tags; not registered
    let DeploymentError::Infeasible(cells) = &err else {
        panic!("expected Infeasible, got: {err:?}");
    };
    let cell = cells
        .iter()
        .find(|c| c.cell == to_sri(CELL_SRI))
        .unwrap_or_else(|| panic!("infeasibility should name cell '{CELL_SRI}', got: {cells:?}"));
    assert!(
        !cell.rejections.is_empty()
            && cell
                .rejections
                .iter()
                .all(|r| matches!(r.reason, RejectionReason::MissingTags(_))),
        "every runtime should be rejected for missing tags, got: {:?}",
        cell.rejections
    );
    assert!(
        assert_ok!(sorg.get_placement(&to_sri(CELL_SRI)).await).is_none(),
        "cell should not be registered after failed deploy"
    );
}

// Case 8: HTTP bridge cell with a tag requirement deploys when the tag is satisfied.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn bridge_cell_with_tag() {
    // Arrange — orch+db, two execs with distinct tags
    let swarm = swarm_config!("cells/orch_only.jsonnet");
    let _exec_gpu = swarm_config!("cells/tagged_placement/exec_gpu.jsonnet");
    let _exec_cpu = swarm_config!("cells/tagged_placement/exec_cpu.jsonnet");

    let test_app = spawn_test_app_with_swarm(swarm).await;
    let sorg = sorg_client::Client::new(test_app.session().clone());

    let bridge_sri = to_sri("tagged_bridge");
    let bridge = CellDeployment::new(
        bridge_sri,
        CellConfig::HttpBridge(HttpBridgeApi {
            cell_name: "tagged_bridge".to_owned(),
            base_url: "http://localhost:9999".to_owned(),
            endpoints: vec![],
        }),
    )
    .with_tags(RequirementTags::new(vec![TAG_GPU]));

    let request = deploy_request(vec![bridge]);

    // Act — deploy an app with a tagged bridge cell. Bridge cells run natively on the
    // orchestrator (no `deployments()` on any runtime to check), but still go through
    // the same tag-matching triage as wasm cells before deploy is attempted — this only
    // succeeds because a gpu-tagged runtime satisfies the requirement.
    assert_ok!(sorg.deploy_cells(request).await);

    // Assert — the bridge is registered natively, keyed by its own sri.
    let entry = assert_ok!(sorg.get_placement(&to_sri("tagged_bridge")).await)
        .expect("bridge cell should be registered after deploy");
    let PlacementKind::Bridge { sri } = &entry.kind else {
        panic!("expected Bridge placement, got {:?}", entry.kind);
    };
    assert_eq!(
        sri, &bridge_sri,
        "bridge cell should be registered under its own sri"
    );
}

// Case 9: standalone deploy with tag [gpu]; R1 has gpu → lands on R1.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn standalone_single_tag_match() {
    // Arrange — orch+db, two execs with distinct tags
    let swarm = swarm_config!("cells/orch_only.jsonnet");
    let _exec_gpu = swarm_config!("cells/tagged_placement/exec_gpu.jsonnet");
    let _exec_cpu = swarm_config!("cells/tagged_placement/exec_cpu.jsonnet");

    build_and_register_cell_class("../../tests/fixtures/dummy_cell", "tagged_cell", &swarm).await;

    let test_app = spawn_test_app_with_swarm(swarm).await;
    let sorg = sorg_client::Client::new(test_app.session().clone());

    let tags = RequirementTags::new(vec![TAG_GPU]);

    // Act — load a standalone cell with a gpu tag requirement
    assert_ok!(
        sorg.deploy_wasm_cell(to_sri(CELL_SRI), "tagged_cell.wasm", tags.clone())
            .await
    );

    // Assert — the cell landed on the gpu runtime
    let entry = assert_ok!(sorg.get_placement(&to_sri(CELL_SRI)).await)
        .expect("standalone cell should be registered after load");
    let node_id = super::assert_wasm_runtime_id(&entry);
    let runtimes = assert_ok!(sorg.list_exec_runtimes().await);
    let expected = find_runtime_matching(&runtimes, &tags);

    assert_eq!(
        node_id, expected,
        "standalone cell should land on the gpu-tagged runtime"
    );
}

// Case 10: standalone deploy requiring absent tag → fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn standalone_unplaceable() {
    // Arrange — orch+db, two execs (gpu + cpu), neither has fpga
    let swarm = swarm_config!("cells/orch_only.jsonnet");
    let _exec_gpu = swarm_config!("cells/tagged_placement/exec_gpu.jsonnet");
    let _exec_cpu = swarm_config!("cells/tagged_placement/exec_cpu.jsonnet");

    build_and_register_cell_class("../../tests/fixtures/dummy_cell", "tagged_cell", &swarm).await;

    let test_app = spawn_test_app_with_swarm(swarm).await;
    let sorg = sorg_client::Client::new(test_app.session().clone());

    // Act — load a standalone cell requiring a tag no runtime has
    let err = assert_err!(
        sorg.deploy_wasm_cell(
            to_sri(CELL_SRI),
            "tagged_cell.wasm",
            RequirementTags::new(vec![TAG_FPGA])
        )
        .await,
        "standalone load should fail when no runtime has the required tag"
    );

    // Assert — infeasible, naming the cell and the unmet tag; cell not registered
    let DeploymentError::Infeasible(cells) = &err else {
        panic!("expected Infeasible, got: {err:?}");
    };
    let cell = cells
        .iter()
        .find(|c| c.cell == to_sri(CELL_SRI))
        .unwrap_or_else(|| panic!("infeasibility should name cell '{CELL_SRI}', got: {cells:?}"));
    assert!(
        cell.rejections.iter().any(|r| matches!(
            &r.reason,
            RejectionReason::MissingTags(tags) if tags.iter().any(|t| t.as_str() == TAG_FPGA)
        )),
        "expected a MissingTags rejection naming '{TAG_FPGA}', got: {:?}",
        cell.rejections
    );
    assert!(
        assert_ok!(sorg.get_placement(&to_sri(CELL_SRI)).await).is_none(),
        "cell should not be registered after failed standalone load"
    );
}
