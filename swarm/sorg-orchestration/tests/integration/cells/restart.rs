//! Root restart across a runtime restart on a single-node swarm
//! (peeriot/myrmic#43): the exec's boot sweep has to report the roots of its
//! previous incarnation, or the orchestrator reads their absence as an
//! operator undeploy and drops the spec.
//!
//! The test talks to the node through a bare zenoh session, like a client
//! would: a swarm node in the test process would observe the old incarnation
//! leave and deregister the exec row the new one has just written.

use std::future::Future;
use std::path::Path;
use std::time::{Duration, Instant};

use cell_protocol::{Gen, Sri, scope_of_cell};
use sorg_client::Client;
use sorg_common::{
    CellConfig, CellDeployment, DbClient, DeployRequest, RestartPolicy, RestartType,
    instance_registry, list_placements, node_lease, root_death, root_restart,
};
use sorg_tests::{
    KillableProcess, killable_swarm_config, register_fixture_class_with, test_session,
};
use zenoh::Session;

use super::lifecycle::{COUNT_KEY, COUNTER_CLASS, COUNTER_LOGIC, COUNTER_SRI, incr_payload};
use crate::integration::to_sri;

/// The node's fixed zenoh id (`single_node_restart.jsonnet`), so every
/// incarnation is the same runtime.
const NODE_ID: &str = "b7e2c4a6d8f0b7e2c4a6d8f0b7e2c4a6";
/// How long the node gets to come up, sweep and lease.
const BOOT_WAIT: Duration = Duration::from_secs(20);
/// A restart takes up to two jittered supervision passes (10 s ±20 % each)
/// plus the redeploy: the boot sweep defers to the exec's per-pass sweep when
/// the db is not up yet, and that pass races the orchestrator's first one.
const RESTART_WAIT: Duration = Duration::from_secs(60);

async fn eventually<F, Fut>(timeout: Duration, mut probe: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let start = Instant::now();
    loop {
        if probe().await {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The clock the exec mints its lease `seq` from.
fn now_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

/// The node's lease sequence, minted from the wall clock on every renewal.
async fn lease_seq(session: &Session) -> Option<u64> {
    node_lease::list_leases(session)
        .await
        .ok()?
        .into_iter()
        .filter(|(id, _)| id.to_string() == NODE_ID)
        .map(|(_, lease)| lease.seq)
        .max()
}

async fn placement_gen(session: &Session, sri: Sri) -> Option<Gen> {
    list_placements(session)
        .await
        .ok()?
        .into_iter()
        .find(|p| p.sri == sri)
        .map(|p| p.gen_id)
}

/// The generation of the cell's instance row, which its init writes.
async fn instance_gen(session: &Session, sri: Sri) -> Option<Gen> {
    instance_registry::get_instance(session, &sri)
        .await
        .ok()
        .flatten()
        .map(|instance| instance.gen_id)
}

/// Starts the node (orch + exec + db) on `db_dir` and waits for a lease minted
/// after `after`, a reading of [`now_ms`]. The exec leases only after its boot
/// sweep had its turn and its registration went through, so a newer lease
/// means this incarnation is up and reachable through the db.
async fn boot_node(db_dir: &Path, session: &Session, after: Option<u64>) -> KillableProcess {
    let overrides = format!("{{ db+: {{ directory: \"{}\" }} }}", db_dir.display());
    let node = killable_swarm_config!("cells/single_node_restart.jsonnet", &overrides);
    assert!(
        eventually(
            BOOT_WAIT,
            || async move { lease_seq(session).await > after }
        )
        .await,
        "node did not lease within {BOOT_WAIT:?}"
    );
    node
}

/// Deploys the counter as a root with `restart: always`.
async fn deploy_counter_root(client: &Client) {
    let class = client
        .resolve_class_name("counter.wasm")
        .await
        .expect("failed to resolve the counter class")
        .expect("counter class is registered");
    let restart = RestartPolicy {
        restart_type: RestartType::Always,
        delay_ms: 200,
        ..RestartPolicy::default()
    };
    let deployment =
        CellDeployment::new(to_sri(COUNTER_SRI), CellConfig::Wasm { class }).with_restart(restart);
    client
        .deploy_cells(DeployRequest::new(vec![deployment]))
        .await
        .expect("failed to deploy the counter root with restart: always");
}

async fn is_registered(client: &Client) -> bool {
    client
        .placement_exists(&to_sri(COUNTER_SRI))
        .await
        .expect("failed to query the counter placement")
}

async fn increment(client: &Client) {
    client
        .command_send(
            to_sri(COUNTER_SRI),
            "increment",
            Some(incr_payload(1)),
            None,
        )
        .await
        .expect("failed to send increment");
}

/// The count the counter root stored in its own KV space, once it has.
async fn stored_count(session: &Session) -> Option<i32> {
    let scope = scope_of_cell(to_sri(COUNTER_SRI));
    let key = COUNT_KEY.to_owned();
    let raw = DbClient::new(session)
        .read_tx_in(scope.clone(), async move |client, tx_id| {
            Ok(sorg_common::key_get(client.clone(), tx_id, scope, key).await)
        })
        .await
        .ok()?
        .ok()??;
    postcard::from_bytes(&raw).ok()
}

async fn count_reaches(session: &Session, want: i32) -> bool {
    eventually(Duration::from_secs(5), || async move {
        stored_count(session).await == Some(want)
    })
    .await
}

async fn restart_specs(session: &Session) -> Vec<Sri> {
    root_restart::list_specs(session)
        .await
        .expect("failed to list restart specs")
        .into_iter()
        .map(|spec| spec.sri)
        .collect()
}

// The only runtime dies with a `restart: always` root on it and comes back:
// its boot sweep reports the root as crashed, so the orchestrator restarts it
// from the spec instead of reading the empty placement as an undeploy.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn restart_always_root_returns_after_single_node_restart() {
    // Arrange - one node on a persistent db, the counter deployed as a restartable root
    let session = test_session().await;
    let client = Client::new(session.clone());
    let db_dir = tempfile::tempdir().expect("failed to create db dir");
    let sri = to_sri(COUNTER_SRI);

    let node = boot_node(db_dir.path(), &session, None).await;
    register_fixture_class_with(COUNTER_LOGIC, COUNTER_CLASS, &session).await;
    deploy_counter_root(&client).await;
    let first_gen = placement_gen(&session, sri)
        .await
        .expect("counter root is placed");
    increment(&client).await;
    assert!(
        count_reaches(&session, 1).await,
        "counter root should process the increment before the restart (count -> 1)"
    );

    // Act - SIGKILL the only runtime and start it again on the same db
    drop(node);
    let _node = boot_node(db_dir.path(), &session, Some(now_ms())).await;

    // Assert I - the root is placed again, at a newer generation
    let session_ref = &session;
    assert!(
        eventually(RESTART_WAIT, || async move {
            placement_gen(session_ref, sri)
                .await
                .is_some_and(|gen_id| gen_id > first_gen)
        })
        .await,
        "root with restart: always was not re-placed within {RESTART_WAIT:?}"
    );

    // Assert II - its spec survived the restart
    assert_eq!(restart_specs(&session).await, vec![sri]);

    // Assert III - the restarted root runs: once its init wrote the instance
    // row, a further increment lands
    assert!(
        eventually(BOOT_WAIT, || async move {
            instance_gen(session_ref, sri)
                .await
                .is_some_and(|gen_id| gen_id > first_gen)
        })
        .await,
        "restarted root did not initialise within {BOOT_WAIT:?}"
    );
    let before = stored_count(&session)
        .await
        .expect("initialised counter root has a count");
    increment(&client).await;
    assert!(
        count_reaches(&session, before + 1).await,
        "restarted counter root should process the increment (count -> {})",
        before + 1
    );
}

// An operator undeploy leaves nothing for the boot sweep to report, so the
// spec stays gone and the root stays down across the runtime restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn undeployed_root_stays_down_after_single_node_restart() {
    // Arrange - the same restartable root, undeployed before the restart
    let session = test_session().await;
    let client = Client::new(session.clone());
    let db_dir = tempfile::tempdir().expect("failed to create db dir");

    let node = boot_node(db_dir.path(), &session, None).await;
    register_fixture_class_with(COUNTER_LOGIC, COUNTER_CLASS, &session).await;
    deploy_counter_root(&client).await;
    assert!(is_registered(&client).await);
    client
        .undeploy_cell(to_sri(COUNTER_SRI))
        .await
        .expect("failed to undeploy the counter root");
    assert!(
        restart_specs(&session).await.is_empty(),
        "undeploy should erase the restart spec"
    );

    // Act - SIGKILL the runtime and start it again
    drop(node);
    let _node = boot_node(db_dir.path(), &session, Some(now_ms())).await;

    // Assert - nothing was reported and the root stays gone
    let deaths = root_death::list(&session)
        .await
        .expect("failed to list root deaths");
    assert!(
        deaths.is_empty(),
        "boot sweep reported a death for an undeployed root: {deaths:?}"
    );
    assert!(restart_specs(&session).await.is_empty());
    assert!(!is_registered(&client).await);
}
