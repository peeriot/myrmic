//! Placement reads three registries - the cell-class registry, the exec
//! registry and the placement table - and each of them lives in its own DB
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

use claims::assert_ok;
use db_commons::models::{
    self, DbRequest, Scope, TxOp, TxOpResponse, locate, tb_get, tb_list, tx_apply, tx_commit,
    tx_rollback,
};
use sorg_common::{
    DeploymentError, RejectionReason, RequirementTags, class_registry, exec_registry,
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
    let result = deploy_probe(&test_app, RequirementTags::new(vec![UNMET_TAG])).await;

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

    // Assert II - deciding that placement had to read the class registry, and
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
    declare_empty_stand_in_holder(&mut test_app, &cell_protocol::class_registry_scope()).await;

    let result = deploy_probe(&test_app, RequirementTags::default()).await;

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
    declare_empty_stand_in_holder(&mut test_app, &cell_protocol::scope_of_exec_registry()).await;

    let result = deploy_probe(&test_app, RequirementTags::default()).await;

    // Assert - placement consulted the holder its own locate picked, found no
    // runtimes there, and said so.
    let err = result.expect_err("the exec registry holder has no rows, so the deploy must fail");
    assert!(
        matches!(err, DeploymentError::NoRuntimesAvailable),
        "expected NoRuntimesAvailable from the located exec-registry holder, got: {err:?}"
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

/// Answers `scope`'s locate round with a holder that has no rows at all.
///
/// The answer claims the highest possible head and a non-draining state, which
/// is the top of the holder ranking for reads and writes alike - so it wins
/// every round for this scope without pinning a node id or leaning on the
/// tie-break draw. Behind it sits a store that serves an empty table for any
/// read and refuses everything else.
async fn declare_empty_stand_in_holder(test_app: &mut TestApp, scope: &Scope) {
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

    test_app
        .set_up_queryable_with_reply(store_topic_of(STAND_IN_HOLDER), |payload| {
            empty_store_reply(&payload)
        })
        .await;
}

/// The stand-in holder's store: transactions open and close, table reads come
/// back empty, and everything else is refused.
fn empty_store_reply(payload: &[u8]) -> Result<Vec<u8>, Vec<u8>> {
    let Ok(request) = postcard::from_bytes::<DbRequest>(payload) else {
        return Err(refusal("unreadable db request"));
    };

    match request {
        DbRequest::TxApply(application) => {
            let tx = match application.target {
                tx_apply::Target::New { .. } => STAND_IN_TX,
                tx_apply::Target::Existing(tx) => tx,
            };
            let last = match application.ops.last() {
                None => None,
                Some(TxOp::TbGet(_)) => Some(TxOpResponse::TbGet(tb_get::Response { value: None })),
                Some(TxOp::TbList(_)) => Some(TxOpResponse::TbList(tb_list::Response {
                    entities: Vec::new(),
                })),
                Some(other) => return Err(refusal(other.name())),
            };
            let open = match application.finish {
                tx_apply::Finish::KeepOpen => Some(tx),
                tx_apply::Finish::Commit => None,
            };
            Ok(encode(&tx_apply::Response { tx: open, last }))
        }
        DbRequest::TxCommit(_) => Ok(encode(&tx_commit::Response {})),
        DbRequest::TxRollback(_) => Ok(encode(&tx_rollback::Response {})),
        other => Err(refusal(other.name())),
    }
}

/// The error the stand-in store answers an operation it does not serve with.
fn refusal(operation: &str) -> Vec<u8> {
    encode(&tx_apply::Error {
        message: format!("this store serves empty reads only, not {operation}"),
        index: None,
    })
}

fn encode<T: serde::Serialize>(value: &T) -> Vec<u8> {
    postcard::to_allocvec(value).expect("the db wire encodes with postcard")
}
