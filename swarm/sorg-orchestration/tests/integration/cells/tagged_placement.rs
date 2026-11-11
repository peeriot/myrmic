use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cell_protocol::{
    BlobHash, CapabilityTag, ExecRuntimeInfo, ExecutionCapabilities, NodeLease, PlacementKind,
    RuntimeId,
};
use claims::{assert_err, assert_ok};
use enum_map::Enum;
use sorg_common::{
    CellConfig, CellDeployment, DeployRequest, DeploymentError, HttpBridgeApi, RejectionReason,
    RequirementTags, exec_registry, node_lease,
};
use sorg_tests::{
    Platform, prebuilt_cell_wasm, register_fixture_class, scope_test_multicast, swarm_config,
};
use test_framework::swarm2::{Local, Swarm, SwarmSpec};

use crate::integration::{spawn_test_app_with_swarm, to_sri};

const APP_NAME: &str = "tag_test_app";
const CELL_SRI: &str = "tagged_cell";

const TAG_GPU: &str = "gpu";
const TAG_CPU: &str = "cpu";
const TAG_SENSOR: &str = "sensor";
const TAG_FPGA: &str = "fpga";

/// the cell crate whose prebuilt wasm the swarm2 tests deploy
const DUMMY_CELL: &str = "../../tests/fixtures/dummy_cell";

fn wasm_cell(sri: &str, class: BlobHash) -> CellDeployment {
    CellDeployment::new(to_sri(sri), CellConfig::Wasm { class })
}

fn deploy_request(cells: Vec<CellDeployment>) -> DeployRequest {
    DeployRequest::new(
        cells
            .into_iter()
            .map(|cell| cell.with_app(Some(APP_NAME.to_owned())))
            .collect(),
    )
}

/// Register the prebuilt `DUMMY_CELL` wasm as a class in `swarm`, for the tests that deploy
/// through the sorg client.
async fn add_dummy_class(swarm: &Swarm<Local, GpuCpu>) -> BlobHash {
    let wasm = std::fs::read(prebuilt_cell_wasm(DUMMY_CELL)).unwrap();
    swarm
        .sorg()
        .add_class("tagged_cell.wasm", wasm, vec![])
        .await
        .unwrap()
}

// Every test holds the swarm's runtime handles in `_runtimes` until it ends: dropping them
// deletes the runtimes.

/// two runtimes, one tagged gpu, one tagged cpu
#[derive(Clone, Copy, PartialEq, Eq, Debug, Enum)]
enum GpuCpu {
    Gpu,
    Cpu,
}

impl SwarmSpec for GpuCpu {
    fn tags(self) -> Vec<String> {
        match self {
            GpuCpu::Gpu => vec![TAG_GPU.to_owned()],
            GpuCpu::Cpu => vec![TAG_CPU.to_owned()],
        }
    }

    fn persistent(self) -> bool {
        false
    }
}

/// two runtimes, one tagged gpu, one tagged gpu and sensor
#[derive(Clone, Copy, PartialEq, Eq, Debug, Enum)]
enum GpuGpuSensor {
    Gpu,
    GpuSensor,
}

impl SwarmSpec for GpuGpuSensor {
    fn tags(self) -> Vec<String> {
        match self {
            GpuGpuSensor::Gpu => vec![TAG_GPU.to_owned()],
            GpuGpuSensor::GpuSensor => vec![TAG_GPU.to_owned(), TAG_SENSOR.to_owned()],
        }
    }

    fn persistent(self) -> bool {
        false
    }
}

// Case 1: cell requires [gpu]; R1 has gpu, R2 has cpu → lands on R1.
#[tokio::test(flavor = "multi_thread")]
async fn single_tag_match() {
    let (swarm, _runtimes) = Swarm::<_, GpuCpu>::start(Local::new()).await.unwrap();

    let cell = swarm
        .client()
        .cell(prebuilt_cell_wasm(DUMMY_CELL))
        .tags(&[TAG_GPU])
        .deploy()
        .await
        .unwrap();

    assert_eq!(swarm.host_of(cell.sri()).await.unwrap(), Some(GpuCpu::Gpu));
    let placed: Vec<String> = swarm
        .placements()
        .await
        .unwrap()
        .iter()
        .map(|entry| entry.sri.to_string())
        .collect();
    assert_eq!(placed, [cell.sri()], "the deploy placed exactly this cell");
}

// Case 2: cell requires [gpu, sensor]; only R1 has both → lands on R1.
#[tokio::test(flavor = "multi_thread")]
async fn multi_tag_and_match() {
    let (swarm, _runtimes) = Swarm::<_, GpuGpuSensor>::start(Local::new()).await.unwrap();

    let cell = swarm
        .client()
        .cell(prebuilt_cell_wasm(DUMMY_CELL))
        .tags(&[TAG_GPU, TAG_SENSOR])
        .deploy()
        .await
        .unwrap();

    assert_eq!(
        swarm.host_of(cell.sri()).await.unwrap(),
        Some(GpuGpuSensor::GpuSensor),
        "the cell lands on the runtime with both tags, not the gpu-only one"
    );
}

// Case 3: cell with no tags deploys onto whatever runtime exists.
#[tokio::test(flavor = "multi_thread")]
async fn untagged_placeable_anywhere() {
    let (swarm, _runtimes) = Swarm::<_, GpuCpu>::start(Local::new()).await.unwrap();

    let cell = swarm
        .client()
        .cell(prebuilt_cell_wasm(DUMMY_CELL))
        .deploy()
        .await
        .unwrap();

    assert!(
        swarm.host_of(cell.sri()).await.unwrap().is_some(),
        "the untagged cell is placed on one of the tagged runtimes"
    );
}

// Case 4: two untagged cells, two runtimes → they spread one-per-node.
#[tokio::test(flavor = "multi_thread")]
async fn untagged_cells_spread_across_nodes() {
    let (swarm, _runtimes) = Swarm::<_, GpuCpu>::start(Local::new()).await.unwrap();
    let class = add_dummy_class(&swarm).await;

    // one request: the planner has to spread within a batch
    let request = deploy_request(vec![wasm_cell("cell_a", class), wasm_cell("cell_b", class)]);
    swarm.sorg().deploy_cells(request).await.unwrap();

    let host_a = swarm.host_of(&to_sri("cell_a").to_string()).await.unwrap();
    let host_b = swarm.host_of(&to_sri("cell_b").to_string()).await.unwrap();
    assert_ne!(
        host_a, host_b,
        "two untagged cells should spread across the two nodes, not consolidate"
    );
}

// Case 5: cell X requires [gpu] (forces R1), cell Y untagged → Y spreads to the empty R2.
#[tokio::test(flavor = "multi_thread")]
async fn untagged_spreads_off_tagged_node() {
    let (swarm, _runtimes) = Swarm::<_, GpuCpu>::start(Local::new()).await.unwrap();
    let class = add_dummy_class(&swarm).await;

    let request = deploy_request(vec![
        wasm_cell("pinned", class).with_tags(RequirementTags::new(vec![TAG_GPU])),
        wasm_cell("free", class),
    ]);
    swarm.sorg().deploy_cells(request).await.unwrap();

    assert_eq!(
        swarm.host_of(&to_sri("pinned").to_string()).await.unwrap(),
        Some(GpuCpu::Gpu),
        "pinned cell should land on the gpu runtime"
    );
    assert_eq!(
        swarm.host_of(&to_sri("free").to_string()).await.unwrap(),
        Some(GpuCpu::Cpu),
        "free cell should spread to the empty cpu node, not pile onto the occupied gpu node"
    );
}

// Case 6: cell requires [fpga], no runtime has it → deploy fails.
#[tokio::test(flavor = "multi_thread")]
async fn unplaceable_tag_absent() {
    // Arrange — two runtimes (gpu + cpu), neither has fpga
    let (swarm, _runtimes) = Swarm::<_, GpuCpu>::start(Local::new()).await.unwrap();
    let class = add_dummy_class(&swarm).await;
    let sorg = swarm.sorg();

    let request = deploy_request(vec![
        wasm_cell(CELL_SRI, class).with_tags(RequirementTags::new(vec![TAG_FPGA])),
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
#[tokio::test(flavor = "multi_thread")]
async fn unplaceable_and_not_satisfiable() {
    // Arrange — two runtimes with one tag each
    let (swarm, _runtimes) = Swarm::<_, GpuCpu>::start(Local::new()).await.unwrap();
    let class = add_dummy_class(&swarm).await;
    let sorg = swarm.sorg();

    let request = deploy_request(vec![
        wasm_cell(CELL_SRI, class).with_tags(RequirementTags::new(vec![TAG_GPU, TAG_CPU])),
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
#[tokio::test(flavor = "multi_thread")]
async fn bridge_cell_with_tag() {
    // Arrange — two runtimes with distinct tags
    let (swarm, _runtimes) = Swarm::<_, GpuCpu>::start(Local::new()).await.unwrap();
    let sorg = swarm.sorg();

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
#[tokio::test(flavor = "multi_thread")]
async fn standalone_single_tag_match() {
    let (swarm, _runtimes) = Swarm::<_, GpuCpu>::start(Local::new()).await.unwrap();
    let class = add_dummy_class(&swarm).await;

    swarm
        .sorg()
        .deploy_wasm_cell(to_sri(CELL_SRI), class, RequirementTags::new(vec![TAG_GPU]))
        .await
        .unwrap();

    assert_eq!(
        swarm.host_of(&to_sri(CELL_SRI).to_string()).await.unwrap(),
        Some(GpuCpu::Gpu),
        "standalone cell should land on the gpu-tagged runtime"
    );
}

// Case 10: standalone deploy requiring absent tag → fails.
#[tokio::test(flavor = "multi_thread")]
async fn standalone_unplaceable() {
    // Arrange — two runtimes (gpu + cpu), neither has fpga
    let (swarm, _runtimes) = Swarm::<_, GpuCpu>::start(Local::new()).await.unwrap();
    let class = add_dummy_class(&swarm).await;
    let sorg = swarm.sorg();

    // Act — load a standalone cell requiring a tag no runtime has
    let err = assert_err!(
        sorg.deploy_wasm_cell(
            to_sri(CELL_SRI),
            class,
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

// Case 11: a peer whose lease `seq` is 5 min behind the leader's wall clock
// is still alive; placement must judge it on the leader's own observation
// clock, not by comparing the peer's `seq` against local time.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn deploy_pinned_to_a_peer_behind_the_leaders_clock_selects_that_peer() {
    const TAG_PEER: &str = "nodea";
    const SKEW_MS: u64 = 300_000;
    const RENEW_EVERY: Duration = Duration::from_secs(2);

    // Arrange - single orchestrating node plus a fake peer with no deploy handler
    let swarm = swarm_config!("cells/orch_only.jsonnet");
    register_fixture_class("../../tests/fixtures/dummy_cell", "tagged_cell", &swarm).await;
    let test_app = spawn_test_app_with_swarm(swarm).await;
    let class = test_app.class_id("tagged_cell.wasm").await;

    let mut config = zenoh::Config::default();
    scope_test_multicast(&mut config);
    let peer_session = zenoh::open(config).await.expect("peer session should open");
    let peer_id = RuntimeId::from(peer_session.zid());
    let info = ExecRuntimeInfo::new(
        peer_session.zid(),
        Some("skewed-peer".to_owned()),
        ExecutionCapabilities::new(
            Platform::Linux
                .get_tags()
                .into_iter()
                .chain([TAG_PEER])
                .map(CapabilityTag::new)
                .collect(),
        ),
    );
    assert_ok!(exec_registry::register_exec(&peer_session, &info).await);

    let now_ms = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after the epoch")
            .as_millis(),
    )
    .expect("millis fit in u64");
    let renewals = tokio::spawn({
        let session = peer_session.clone();
        async move {
            // Strictly increasing seq, always about 5 min in the leader's past.
            let mut n = 0u64;
            loop {
                let lease = NodeLease {
                    device_id: peer_id.to_string(),
                    seq: now_ms - SKEW_MS + n * 2_000,
                    ttl_ms: 45_000,
                };
                node_lease::renew_lease(&session, peer_id, &lease, Duration::from_secs(3_600))
                    .await
                    .expect("fake peer lease renewal should succeed");
                tokio::time::sleep(RENEW_EVERY).await;
                n += 1;
            }
        }
    });

    let mut client_config = sorg_client::Config::default();
    client_config.set_query_timeout(Duration::from_secs(5));
    let sorg = sorg_client::Client::new_with_config(test_app.session().clone(), client_config);

    // Act - deploy a cell pinned to the peer's tag until placement stops
    // rejecting it; the first reads see the peer's seq before it advances
    let deadline = Instant::now() + Duration::from_secs(40);
    let err = loop {
        let request = deploy_request(vec![
            wasm_cell(CELL_SRI, class).with_tags(RequirementTags::new(vec![TAG_PEER])),
        ]);
        let err = assert_err!(
            sorg.deploy_cells(request).await,
            "the fake peer has no deploy handler"
        );
        let rejected_at_placement = matches!(
            err,
            DeploymentError::Infeasible(_) | DeploymentError::NoRuntimesAvailable
        );
        if !rejected_at_placement || Instant::now() >= deadline {
            break err;
        }
    };
    renewals.abort();

    // Assert - placement selected the peer, so the deploy fails only in the
    // deploy phase (the fake peer never answers), not at placement
    assert!(
        matches!(&err, DeploymentError::DeploymentFailed(failures)
            if !failures.is_empty() && failures.iter().all(|failure| failure.runtime == peer_id)),
        "placement must select the live peer despite its lagging lease seq; \
         expected a deploy-phase failure, got: {err:?}"
    );
}
