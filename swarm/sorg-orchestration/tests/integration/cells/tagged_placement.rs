use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use cell_protocol::{
    CapabilityTag, ExecRuntimeInfo, ExecutionCapabilities, NodeLease, PlacementKind, RuntimeId,
};
use claims::{assert_err, assert_ok};
use sorg_common::{
    CellConfig, CellDeployment, DeployRequest, DeploymentError, HttpBridgeApi, RejectionReason,
    RequirementTags, exec_registry, node_lease,
};
use sorg_tests::{
    Platform, prebuilt_cell_wasm, register_fixture_class, scope_test_multicast, swarm_config,
};
use test_framework::{
    World,
    myrmic::{self, Error},
    overlay::Overlay,
    runtime::Running,
    start_runtimes,
    swarm::{
        RegisterError, RunningRt, Swarm,
        cell::{self, Deployment},
    },
};

use crate::integration::{spawn_test_app_with_swarm, to_sri};

const APP_NAME: &str = "tag_test_app";
const CELL_SRI: &str = "tagged_cell";

const TAG_GPU: &str = "gpu";
const TAG_CPU: &str = "cpu";
const TAG_SENSOR: &str = "sensor";
const TAG_FPGA: &str = "fpga";

/// the cell crate whose prebuilt wasm the tests deploy
const DUMMY_CELL: &str = "../../tests/fixtures/dummy_cell";

/// Register the prebuilt `DUMMY_CELL` wasm as a class in `swarm`, for the tests that deploy
/// through the sorg client.
async fn register_dummy(swarm: &Swarm) -> Result<cell::Class, RegisterError> {
    swarm
        .register(&cell::Binaries::from_file(prebuilt_cell_wasm(DUMMY_CELL)))
        .await
}

// Every test holds the swarm's runtime handles until it ends: dropping them deletes the
// runtimes.

/// a running CLI runtime of a swarm
type CliRt = RunningRt<myrmic::RunningRt<myrmic::Local>>;

/// A swarm of two CLI runtimes on one group, tagged `first` and `second`, started; their handles
/// in that order.
async fn two_runtimes(first: &[&str], second: &[&str]) -> Result<(Swarm, [CliRt; 2]), Error> {
    let group = Overlay::new(&World::local()).new_multicast_group();
    let swarm = Swarm::new(&group).await;
    let first = swarm.add(swarm.get_myrmic().runtime().tags(first));
    let second = swarm.add(swarm.get_myrmic().runtime().tags(second));
    start_runtimes!(swarm; first, second);
    Ok((swarm, [first, second]))
}

/// two runtimes, one tagged gpu, one tagged cpu
async fn gpu_cpu() -> Result<(Swarm, [CliRt; 2]), Error> {
    two_runtimes(&[TAG_GPU], &[TAG_CPU]).await
}

/// two runtimes, one tagged gpu, one tagged gpu and sensor
async fn gpu_gpu_sensor() -> Result<(Swarm, [CliRt; 2]), Error> {
    two_runtimes(&[TAG_GPU], &[TAG_GPU, TAG_SENSOR]).await
}

// Case 1: cell requires [gpu]; R1 has gpu, R2 has cpu → lands on R1.
#[tokio::test(flavor = "multi_thread")]
async fn single_tag_match() -> anyhow::Result<()> {
    let (swarm, [gpu, _cpu]) = gpu_cpu().await?;

    let cell = swarm
        .get_myrmic()
        .cell(prebuilt_cell_wasm(DUMMY_CELL))
        .tags(&[TAG_GPU])
        .deploy()
        .await?;

    assert_eq!(swarm.get_host_of(cell.sri()).await?, Some(gpu.get_zid()));
    let placed: Vec<String> = swarm
        .get_sorg()
        .list_placements()
        .await?
        .iter()
        .map(|entry| entry.sri.to_string())
        .collect();
    assert_eq!(placed, [cell.sri()], "the deploy placed exactly this cell");
    Ok(())
}

// Case 2: cell requires [gpu, sensor]; only R1 has both → lands on R1.
#[tokio::test(flavor = "multi_thread")]
async fn multi_tag_and_match() -> anyhow::Result<()> {
    let (swarm, [_gpu, gpu_sensor]) = gpu_gpu_sensor().await?;

    let cell = swarm
        .get_myrmic()
        .cell(prebuilt_cell_wasm(DUMMY_CELL))
        .tags(&[TAG_GPU, TAG_SENSOR])
        .deploy()
        .await?;

    assert_eq!(
        swarm.get_host_of(cell.sri()).await?,
        Some(gpu_sensor.get_zid()),
        "the cell lands on the runtime with both tags, not the gpu-only one"
    );
    Ok(())
}

// Case 3: cell with no tags deploys onto whatever runtime exists.
#[tokio::test(flavor = "multi_thread")]
async fn untagged_placeable_anywhere() -> anyhow::Result<()> {
    let (swarm, _runtimes) = gpu_cpu().await?;

    let cell = swarm
        .get_myrmic()
        .cell(prebuilt_cell_wasm(DUMMY_CELL))
        .deploy()
        .await?;

    assert!(
        swarm.get_host_of(cell.sri()).await?.is_some(),
        "the untagged cell is placed on one of the tagged runtimes"
    );
    Ok(())
}

// Case 4: two untagged cells, two runtimes → they spread one-per-node.
#[tokio::test(flavor = "multi_thread")]
async fn untagged_cells_spread_across_nodes() -> anyhow::Result<()> {
    let (swarm, _runtimes) = gpu_cpu().await?;
    let class = register_dummy(&swarm).await?;

    // one request: the planner has to spread within a batch
    swarm
        .deploy_all(&[
            Deployment {
                class: &class,
                sri: to_sri("cell_a"),
                tags: &[],
            },
            Deployment {
                class: &class,
                sri: to_sri("cell_b"),
                tags: &[],
            },
        ])
        .await?;

    let host_a = swarm
        .get_host_of(&to_sri("cell_a").to_string())
        .await?
        .context("cell_a has no host")?;
    let host_b = swarm
        .get_host_of(&to_sri("cell_b").to_string())
        .await?
        .context("cell_b has no host")?;
    assert_ne!(
        host_a, host_b,
        "two untagged cells should spread across the two nodes, not consolidate"
    );
    Ok(())
}

// Case 5: cell X requires [gpu] (forces R1), cell Y untagged → Y spreads to the empty R2.
#[tokio::test(flavor = "multi_thread")]
async fn untagged_spreads_off_tagged_node() -> anyhow::Result<()> {
    let (swarm, [gpu, cpu]) = gpu_cpu().await?;
    let class = register_dummy(&swarm).await?;

    swarm
        .deploy_all(&[
            Deployment {
                class: &class,
                sri: to_sri("pinned"),
                tags: &[TAG_GPU],
            },
            Deployment {
                class: &class,
                sri: to_sri("free"),
                tags: &[],
            },
        ])
        .await?;

    assert_eq!(
        swarm.get_host_of(&to_sri("pinned").to_string()).await?,
        Some(gpu.get_zid()),
        "pinned cell should land on the gpu runtime"
    );
    assert_eq!(
        swarm.get_host_of(&to_sri("free").to_string()).await?,
        Some(cpu.get_zid()),
        "free cell should spread to the empty cpu node, not pile onto the occupied gpu node"
    );
    Ok(())
}

// Case 6: cell requires [fpga], no runtime has it → deploy fails.
#[tokio::test(flavor = "multi_thread")]
async fn unplaceable_tag_absent() -> anyhow::Result<()> {
    // Arrange — two runtimes (gpu + cpu), neither has fpga
    let (swarm, _runtimes) = gpu_cpu().await?;
    let class = register_dummy(&swarm).await?;
    let sorg = swarm.get_sorg();

    // Act — deploy a cell requiring a tag no runtime has
    let err = assert_err!(
        swarm
            .deploy_all(&[Deployment {
                class: &class,
                sri: to_sri(CELL_SRI),
                tags: &[TAG_FPGA],
            }])
            .await,
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
        sorg.get_placement(&to_sri(CELL_SRI)).await?.is_none(),
        "cell should not be registered after failed deploy"
    );
    Ok(())
}

// Case 7: cell requires [gpu, cpu]; R1 has gpu, R2 has cpu, none has both → fail.
#[tokio::test(flavor = "multi_thread")]
async fn unplaceable_and_not_satisfiable() -> anyhow::Result<()> {
    // Arrange — two runtimes with one tag each
    let (swarm, _runtimes) = gpu_cpu().await?;
    let class = register_dummy(&swarm).await?;
    let sorg = swarm.get_sorg();

    // Act — deploy a cell requiring both tags (no single runtime has both)
    let err = assert_err!(
        swarm
            .deploy_all(&[Deployment {
                class: &class,
                sri: to_sri(CELL_SRI),
                tags: &[TAG_GPU, TAG_CPU],
            }])
            .await,
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
        sorg.get_placement(&to_sri(CELL_SRI)).await?.is_none(),
        "cell should not be registered after failed deploy"
    );
    Ok(())
}

// Case 8: HTTP bridge cell with a tag requirement deploys when the tag is satisfied.
#[tokio::test(flavor = "multi_thread")]
async fn bridge_cell_with_tag() -> anyhow::Result<()> {
    // Arrange — two runtimes with distinct tags
    let (swarm, _runtimes) = gpu_cpu().await?;
    let sorg = swarm.get_sorg();

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

    let request = DeployRequest::new(vec![bridge.with_app(Some(APP_NAME.to_owned()))]);

    // Act — deploy an app with a tagged bridge cell. Bridge cells run natively on the
    // orchestrator (no `deployments()` on any runtime to check), but still go through
    // the same tag-matching triage as wasm cells before deploy is attempted — this only
    // succeeds because a gpu-tagged runtime satisfies the requirement.
    sorg.deploy_cells(request).await?;

    // Assert — the bridge is registered natively, keyed by its own sri.
    let entry = sorg
        .get_placement(&to_sri("tagged_bridge"))
        .await?
        .expect("bridge cell should be registered after deploy");
    let PlacementKind::Bridge { sri } = &entry.kind else {
        panic!("expected Bridge placement, got {:?}", entry.kind);
    };
    assert_eq!(
        sri, &bridge_sri,
        "bridge cell should be registered under its own sri"
    );
    Ok(())
}

// Case 9: standalone deploy with tag [gpu]; R1 has gpu → lands on R1.
#[tokio::test(flavor = "multi_thread")]
async fn standalone_single_tag_match() -> anyhow::Result<()> {
    let (swarm, [gpu, _cpu]) = gpu_cpu().await?;
    let class = register_dummy(&swarm).await?;

    swarm
        .deploy(&class, to_sri(CELL_SRI), &[TAG_GPU], None)
        .await?;

    assert_eq!(
        swarm.get_host_of(&to_sri(CELL_SRI).to_string()).await?,
        Some(gpu.get_zid()),
        "standalone cell should land on the gpu-tagged runtime"
    );
    Ok(())
}

// Case 10: standalone deploy requiring absent tag → fails.
#[tokio::test(flavor = "multi_thread")]
async fn standalone_unplaceable() -> anyhow::Result<()> {
    // Arrange — two runtimes (gpu + cpu), neither has fpga
    let (swarm, _runtimes) = gpu_cpu().await?;
    let class = register_dummy(&swarm).await?;
    let sorg = swarm.get_sorg();

    // Act — load a standalone cell requiring a tag no runtime has
    let err = assert_err!(
        swarm
            .deploy(&class, to_sri(CELL_SRI), &[TAG_FPGA], None)
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
        sorg.get_placement(&to_sri(CELL_SRI)).await?.is_none(),
        "cell should not be registered after failed standalone load"
    );
    Ok(())
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
        let request = DeployRequest::new(vec![
            CellDeployment::new(to_sri(CELL_SRI), CellConfig::Wasm { class })
                .with_tags(RequirementTags::new(vec![TAG_PEER]))
                .with_app(Some(APP_NAME.to_owned())),
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
