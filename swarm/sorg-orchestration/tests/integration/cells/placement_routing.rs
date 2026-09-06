//! Placement makes four reads - the cell-class registry, the exec registry, the
//! node leases and the placement table - and each of them lives in its own DB
//! scope. A scope is resolved to a holder by its own locate round on its own
//! keyexpr, so a read addressed through some other scope's holder can be
//! answered by a node that does not have the rows.
//!
//! These tests watch and answer those locate rounds on the wire. The swarm runs
//! in-process on the session the test holds, and zenoh delivers a session's own
//! queries to its own queryables, so a queryable declared here sits beside the
//! real holder's and takes part in the same round. That makes "which scope did
//! this read address?" observable from outside the orchestrator, on a single
//! real node, with no pinned node ids and no second db.
//!
//! A stand-in holder here holds exactly one scope and refuses any operation
//! naming another, which is what keeps these tests from proving each other's
//! property: an empty exec registry and an empty lease table both end a deploy
//! in `NoRuntimesAvailable`, so without that refusal a read that wandered into
//! the wrong stand-in would look like the outcome the test wants.

use std::time::{Duration, Instant};

use cell_protocol::ClassInfo;
use claims::assert_ok;
use db_commons::models::{
    self, DbRequest, Scope, TxOp, TxOpResponse, locate, tb_get, tb_list, tx_apply, tx_commit,
    tx_rollback,
};
use sorg_common::{
    DeploymentError, RejectionReason, RequirementTags, class_registry, exec_registry, node_lease,
};
use sorg_tests::{TestApp, build_and_register_cell_class, swarm_config};
use zenoh::key_expr::OwnedKeyExpr;

use crate::integration::{spawn_test_app_with_swarm, to_sri};

const PROBE_CELL_CRATE: &str = "../../tests/fixtures/dummy_cell";
const PROBE_CLASS_STEM: &str = "routing_probe";
const PROBE_CLASS: &str = "routing_probe.wasm";
const PROBE_SRI: &str = "routing_probe_cell";

/// A tag no runtime in the fixture carries, so a cell requiring it is rejected
/// while the placement decision is still being made.
const UNMET_TAG: &str = "fpga";

/// The stand-in holder's node id. Every byte is non-zero, so its `uhlc::ID`
/// renders in full and the store keyexpr derived from it is stable.
const STAND_IN_HOLDER: models::NodeId = [7u8; 16];

/// The transaction the stand-in holder hands out. Its third element must be the
/// holder's own id: that is what a client direct-routes later ops to.
const STAND_IN_TX: models::TxId = (1, 1, STAND_IN_HOLDER);

/// What placement sleeps in total before giving up on an artifact-blocked
/// outcome, mirroring the backoff table it retries on. Deliberately restated
/// rather than imported - the constant is private to the orchestrator, and a
/// test that read it could not notice the table shrinking to nothing.
///
/// It is the only thing separating a retried outcome from one returned at once,
/// so the timed tests below assert both sides of it: an artifact-blocked deploy
/// takes at least this long, while an unheld tag and an unknown class come back
/// inside it. Nothing on the deploy path waits anywhere else, which is what the
/// fast side proves.
const RETRY_BUDGET: Duration = Duration::from_millis(3000);

// Deciding a placement requires the class registry, and reading the class
// registry means locating it. Anything else is reading it through another
// scope's holder, which is only correct by accident.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn placement_locates_the_class_registry_scope() {
    // Arrange - one node running orchestration, execution and db, with the
    // probe class registered on it.
    let swarm = swarm_config!("cells/cells.jsonnet");
    build_and_register_cell_class(PROBE_CELL_CRATE, PROBE_CLASS_STEM, &swarm).await;

    let mut test_app = spawn_test_app_with_swarm(swarm).await;

    // A recorder on the class registry's locate keyexpr. It never replies and
    // drops every query, so the real holder still wins the round untouched -
    // the only thing that changes is that the round becomes countable.
    let locate_topic = locate_topic_of(&cell_protocol::class_registry_scope());
    test_app.set_up_queryable(locate_topic.clone()).await;

    // Registering the class and starting the swarm located this scope already.
    // Drop those, so anything counted below belongs to the deploy.
    test_app
        .received_query_payloads_on_topic(locate_topic.clone())
        .await;

    // Act - deploy the registered class demanding a tag no runtime has. The tag
    // is what keeps the deploy inside placement: on a deploy that gets through,
    // the exec's own module load reads the class registry too, and its locate
    // would be indistinguishable from placement's.
    let started = Instant::now();
    let result = deploy_probe(&test_app, RequirementTags::new(vec![UNMET_TAG])).await;
    let elapsed = started.elapsed();

    // Assert I - the deploy really did stop at the placement decision, on the
    // tag, with the class found. Anything else and the count below means
    // something other than what this test claims.
    let err = result.expect_err("a cell requiring an unheld tag must not be placed");
    let DeploymentError::Infeasible(cells) = &err else {
        panic!("expected the deploy to stop at placement with Infeasible, got: {err:?}");
    };
    assert!(
        cells
            .iter()
            .any(|cell| cell.rejections.iter().any(|r| matches!(
                &r.reason,
                RejectionReason::MissingTags(tags) if tags.iter().any(|t| t == UNMET_TAG)
            ))),
        "expected every rejection to be about the unheld tag '{UNMET_TAG}', got: {cells:?}"
    );

    // Assert II - and it came back at once. Waiting cannot grow a tag onto a
    // runtime, so an unheld tag is not an outcome placement retries; spending
    // the artifact budget on it would burn a caller's deadline for nothing.
    assert!(
        elapsed < RETRY_BUDGET,
        "an unplaceable tag came back after {elapsed:?}, past the {RETRY_BUDGET:?} \
         placement spends on an artifact - it was retried"
    );

    // Assert III - deciding that placement had to read the class registry, and
    // reading it has to address the class registry's own scope.
    let locates = test_app
        .received_query_payloads_on_topic(locate_topic.clone())
        .await;
    assert!(
        !locates.is_empty(),
        "placement decided without a single locate on '{locate_topic}': \
         the class read was addressed through some other scope's holder"
    );
}

// Locating the class registry is not enough - the read has to be answered by
// the holder that locate picked. A stand-in holder that wins the round and has
// no rows must make the deploy report the class as unknown.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn placement_class_read_follows_the_class_registry_holder() {
    // Arrange - the probe class registered on the one real node.
    let swarm = swarm_config!("cells/cells.jsonnet");
    build_and_register_cell_class(PROBE_CELL_CRATE, PROBE_CLASS_STEM, &swarm).await;

    let mut test_app = spawn_test_app_with_swarm(swarm).await;

    // Guard against a vacuous pass: an unregistered class fails the same way
    // this test asserts. Read the class through its own scope while the real
    // holder is still the only answer, so the failure below can only come from
    // the stand-in.
    let registered =
        assert_ok!(class_registry::get_class_info(test_app.session(), PROBE_CLASS).await);
    assert!(
        registered.is_some(),
        "the probe class must be readable on the class registry's own holder \
         before the stand-in exists, otherwise this test proves nothing"
    );

    // Act - put an empty stand-in holder in front of the class registry and
    // deploy normally.
    declare_stand_in_holder(&mut test_app, &cell_protocol::class_registry_scope(), None).await;

    let started = Instant::now();
    let result = deploy_probe(&test_app, RequirementTags::default()).await;
    let elapsed = started.elapsed();

    // Assert - placement consulted the holder its own locate picked, found no
    // row there, and said so.
    let err = result.expect_err("the class registry holder has no rows, so the deploy must fail");
    let DeploymentError::UnknownClass { class } = &err else {
        panic!("expected UnknownClass from the located class-registry holder, got: {err:?}");
    };
    assert_eq!(
        class, PROBE_CLASS,
        "the error must name the class that was read"
    );

    // And it said so at once. An absent class is the diagnosis a user acts on,
    // so it must not be held back behind the artifact budget: waiting cannot
    // put a row on a holder that never had one, and past a caller's deadline
    // the wait replaces the name of the class with a timeout.
    assert!(
        elapsed < RETRY_BUDGET,
        "an unknown class came back after {elapsed:?}, past the {RETRY_BUDGET:?} \
         placement spends on an artifact - it was retried"
    );
}

// The same property for the exec registry, which lives in a scope of its own
// and is read by the same placement decision.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn placement_exec_registry_read_follows_its_own_scope() {
    // Arrange - the probe class registered on the one real node.
    let swarm = swarm_config!("cells/cells.jsonnet");
    build_and_register_cell_class(PROBE_CELL_CRATE, PROBE_CLASS_STEM, &swarm).await;

    let mut test_app = spawn_test_app_with_swarm(swarm).await;

    // Guard against a vacuous pass: a genuinely empty exec registry yields the
    // same error this test asserts. Wait for the node's own exec to register
    // and confirm the registry is non-empty while the real holder is still the
    // only answer.
    let own_runtime = test_app.runtime_id().to_string();
    test_app.wait_for_registered_exec(&own_runtime).await;
    let execs = assert_ok!(exec_registry::list_registered_execs(test_app.session()).await);
    assert!(
        !execs.is_empty(),
        "the exec registry must be non-empty on its own holder before the \
         stand-in exists, otherwise this test proves nothing"
    );

    // Act - put an empty stand-in holder in front of the exec registry and
    // deploy normally.
    declare_stand_in_holder(
        &mut test_app,
        &cell_protocol::scope_of_exec_registry(),
        None,
    )
    .await;

    let result = deploy_probe(&test_app, RequirementTags::default()).await;

    // Assert - placement consulted the holder its own locate picked, found no
    // runtimes there, and said so. The lease read cannot be what produced this:
    // it names its own scope, and this stand-in refuses an op belonging to
    // another, so a lease read arriving here would have failed the deploy
    // internally instead.
    let err = result.expect_err("the exec registry holder has no rows, so the deploy must fail");
    assert!(
        matches!(err, DeploymentError::NoRuntimesAvailable),
        "expected NoRuntimesAvailable from the located exec-registry holder, got: {err:?}"
    );
}

// The fourth read. Placement drops every exec with no lease row, so the node
// leases decide the fleet just as much as the exec registry does - and a lease
// scope answered by a holder without the rows must empty a fleet whose exec
// registry is intact.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn placement_node_lease_read_follows_its_own_scope() {
    // Arrange - the probe class registered on the one real node.
    let swarm = swarm_config!("cells/cells.jsonnet");
    build_and_register_cell_class(PROBE_CELL_CRATE, PROBE_CLASS_STEM, &swarm).await;

    let mut test_app = spawn_test_app_with_swarm(swarm).await;

    // Guard against a vacuous pass twice over: a node that never leased, and a
    // node that never registered, both end the deploy the same way. Confirm
    // both rows on their real holders while those are still the only answer.
    let own_runtime = test_app.runtime_id().to_string();
    test_app.wait_for_registered_exec(&own_runtime).await;
    let leases = assert_ok!(node_lease::list_leases(test_app.session()).await);
    assert!(
        !leases.is_empty(),
        "the node must hold a lease on the lease scope's own holder before the \
         stand-in exists, otherwise this test proves nothing"
    );

    // Act - put a stand-in holder with no lease rows in front of the lease
    // scope and deploy normally. It swallows the node's own renewals too, which
    // costs nothing here: a renewal failure is logged and dropped, the row it
    // would have refreshed still sits on the real holder, and hygiene tears
    // down only nodes that are hosting cells - none are.
    declare_stand_in_holder(&mut test_app, &cell_protocol::node_lease_scope(), None).await;

    let result = deploy_probe(&test_app, RequirementTags::default()).await;

    // Assert - the exec registry still lists the node, and placement dropped it
    // anyway, because the holder its lease locate picked had no lease for it.
    // Had that read gone anywhere else it would have found the real lease and
    // the deploy would have gone through; had the exec read landed here instead
    // it would have been refused as an op for another scope, not answered
    // empty.
    let err = result.expect_err("the lease holder has no rows, so every exec is dropped");
    assert!(
        matches!(err, DeploymentError::NoRuntimesAvailable),
        "expected NoRuntimesAvailable from the located lease holder, got: {err:?}"
    );
}

// The retry is the one thing placement does with time, and it is worth its
// budget only if it runs. A class row that exists and carries no artifact at
// all holds the deploy in the single outcome a fresh read could change, so the
// deploy has to spend the whole budget before reporting it.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn placement_retries_while_only_an_artifact_is_missing() {
    // Arrange - the probe class registered on the one real node.
    let swarm = swarm_config!("cells/cells.jsonnet");
    build_and_register_cell_class(PROBE_CELL_CRATE, PROBE_CLASS_STEM, &swarm).await;

    let mut test_app = spawn_test_app_with_swarm(swarm).await;

    // Guard against a vacuous pass: the class carries its wasm artifact on its
    // own holder before the stand-in exists, so the rejection below can only
    // come from the artifact-less row the stand-in serves.
    let registered =
        assert_ok!(class_registry::get_class_info(test_app.session(), PROBE_CLASS).await)
            .expect("the probe class must be readable before the stand-in exists");
    assert!(
        registered.wasm_hash.is_some(),
        "the probe class must carry its wasm artifact on its own holder, \
         otherwise this test proves nothing"
    );

    // Act - a stand-in serving a class row with no artifacts at all. The class
    // is found, so this is not an unknown class; no runtime can load it, so
    // every rejection names the artifact; and nothing will ever change that, so
    // the deploy runs the retry to the end.
    declare_stand_in_holder(
        &mut test_app,
        &cell_protocol::class_registry_scope(),
        Some(encode(&ClassInfo {
            name: PROBE_CLASS.to_owned(),
            wasm_hash: None,
            artifacts: Vec::new(),
        })),
    )
    .await;

    let started = Instant::now();
    let result = deploy_probe(&test_app, RequirementTags::default()).await;
    let elapsed = started.elapsed();

    // Assert - the outcome names the artifact, and getting it took the whole
    // backoff, which only a placement that tried again can spend. The unheld-tag
    // and unknown-class tests above take the same measurement on outcomes that
    // must not be retried, so this is the difference the retry makes and not the
    // cost of a deploy.
    let err = result.expect_err("a class carrying no artifact cannot be placed");
    let DeploymentError::Infeasible(cells) = &err else {
        panic!("expected Infeasible with a missing-artifact rejection, got: {err:?}");
    };
    assert!(
        cells.iter().all(|cell| cell
            .rejections
            .iter()
            .any(|r| matches!(r.reason, RejectionReason::MissingArtifact(_)))),
        "expected every cell to be blocked by a missing artifact, got: {cells:?}"
    );
    assert!(
        elapsed >= RETRY_BUDGET,
        "an artifact-blocked deploy came back after {elapsed:?}, inside the \
         {RETRY_BUDGET:?} placement spends on one - it was not retried"
    );
}

/// Deploys the probe cell through a client of the test's own, which carries the
/// client default deadline rather than the shorter one the shared test app
/// pins, and - unlike the shared app's deploy helper - can carry tags.
async fn deploy_probe(test_app: &TestApp, tags: RequirementTags) -> Result<(), DeploymentError> {
    sorg_client::Client::new(test_app.session().clone())
        .deploy_wasm_cell(to_sri(PROBE_SRI), PROBE_CLASS, tags)
        .await
}

/// The keyexpr a scope's locate round runs on. Derived from the scope rather
/// than spelled out, so renaming either side breaks this loudly.
fn locate_topic_of(scope: &Scope) -> OwnedKeyExpr {
    OwnedKeyExpr::new(db_commons::topics::replica_query::format(
        &scope.namespace,
        &scope.database,
        &scope.schema,
    ))
    .expect("a scope's locate keyexpr is well formed")
}

/// The keyexpr a node serves its store on. A client that has located a node
/// sends every later request straight here.
fn store_topic_of(node: models::NodeId) -> OwnedKeyExpr {
    let id = uhlc::ID::try_from(&node).expect("a node id is a valid uhlc id");
    OwnedKeyExpr::new(db_commons::topics::format_query(id))
        .expect("a node's store keyexpr is well formed")
}

/// Answers `scope`'s locate round with a holder that holds `scope` and nothing
/// else. `row` is what its point reads return - `None` for a holder with no
/// rows at all, `Some(bytes)` for one serving a single canned value.
///
/// The answer claims the highest possible head and a non-draining state, which
/// is the top of the holder ranking for reads and writes alike - so it wins
/// every round for this scope without pinning a node id or leaning on the
/// tie-break draw.
///
/// Behind it sits a store that serves reads of `scope` - a point read answers
/// `row`, a list answers empty - and refuses everything else, an operation
/// naming a different scope included. That refusal is load-bearing: an op
/// carries its own scope whatever holder it reaches, so a read that belongs
/// elsewhere and lands here fails the deploy with an internal error instead of
/// passing for an empty registry and standing in for the property under test.
async fn declare_stand_in_holder(test_app: &mut TestApp, scope: &Scope, row: Option<Vec<u8>>) {
    test_app
        .set_up_queryable_with_reply(locate_topic_of(scope), |_| {
            Ok(encode(&locate::Response {
                id: STAND_IN_HOLDER,
                head: u64::MAX,
                peers: Vec::new(),
                state: locate::HolderState::Replica,
            }))
        })
        .await;

    let held = scope.clone();
    test_app
        .set_up_queryable_with_reply(store_topic_of(STAND_IN_HOLDER), move |payload| {
            stand_in_store_reply(&payload, &held, row.as_deref())
        })
        .await;
}

/// The stand-in holder's store: transactions open and close, reads of the scope
/// it holds are answered from `row`, and everything else is refused. Each read
/// on these paths sends one op per request, so guarding the last one guards the
/// request.
fn stand_in_store_reply(
    payload: &[u8],
    held: &Scope,
    row: Option<&[u8]>,
) -> Result<Vec<u8>, Vec<u8>> {
    let Ok(request) = postcard::from_bytes::<DbRequest>(payload) else {
        return Err(refusal("an unreadable db request", held));
    };

    match request {
        DbRequest::TxApply(application) => {
            let tx = match application.target {
                tx_apply::Target::New { .. } => STAND_IN_TX,
                tx_apply::Target::Existing(tx) => tx,
            };
            let last = match application.ops.last() {
                None => None,
                Some(TxOp::TbGet(op)) if op.scope == *held => {
                    Some(TxOpResponse::TbGet(tb_get::Response {
                        value: row.map(<[u8]>::to_vec),
                    }))
                }
                Some(TxOp::TbList(op)) if op.scope == *held => {
                    Some(TxOpResponse::TbList(tb_list::Response {
                        entities: Vec::new(),
                    }))
                }
                Some(other) => return Err(refusal(other.name(), held)),
            };
            let open = match application.finish {
                tx_apply::Finish::KeepOpen => Some(tx),
                tx_apply::Finish::Commit => None,
            };
            Ok(encode(&tx_apply::Response { tx: open, last }))
        }
        DbRequest::TxCommit(_) => Ok(encode(&tx_commit::Response {})),
        DbRequest::TxRollback(_) => Ok(encode(&tx_rollback::Response {})),
        other => Err(refusal(other.name(), held)),
    }
}

/// The error the stand-in store answers with. It holds one scope, so both an
/// operation it does not serve and a read belonging to another scope land here.
fn refusal(operation: &str, held: &Scope) -> Vec<u8> {
    encode(&tx_apply::Error {
        message: format!(
            "this store holds '{}' and serves reads of it only, not {operation}",
            held.database
        ),
        index: None,
    })
}

fn encode<T: serde::Serialize>(value: &T) -> Vec<u8> {
    postcard::to_allocvec(value).expect("the db wire encodes with postcard")
}
