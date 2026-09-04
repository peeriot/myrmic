use std::time::Duration;

use cell_protocol::{BlobHash, Sri};
use claims::{assert_ok, assert_some};
use myrmic_common::cells::{ClassRef, SpawnRequest};
use sorg_client::Client;
use sorg_tests::{TestApp, build_and_register_cell_class, swarm_config};

use crate::integration::spawn_test_app_with_swarm;

const SPAWNER_SRN: &str = "spawner_cell";
const CHILD_LOCAL: &str = "spawned_child";
/// Event the child publishes its stored value on. Commands are fire-and-forget,
/// so the value can no longer be returned from a `get_value` reply.
const CHILD_VALUE_EVENT: &str = "child_value";

/// The spawner is deployed under its deterministic SRI (derived from its SRN),
/// so its identity — and therefore the SRIs of the children it spawns — are
/// predictable from the derivation alone.
fn spawner_sri() -> Sri {
    Sri::of_path(SPAWNER_SRN).unwrap()
}

/// The SRI the host assigns to the child the spawner spawns under `CHILD_LOCAL`
/// (`child_sri(spawner_identity, CHILD_LOCAL)`).
fn expected_child_sri() -> Sri {
    spawner_sri().child(CHILD_LOCAL).unwrap()
}

/// Builds and registers both cell classes, deploys the spawner, and returns
/// the test app and a client for further queries.
async fn setup() -> (TestApp, Client) {
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class(
        "../../tests/fixtures/cell-spawn-child",
        "spawn_child",
        &swarm,
    )
    .await;
    build_and_register_cell_class("../../tests/fixtures/cell-spawner", "spawner", &swarm).await;
    let test_app = spawn_test_app_with_swarm(swarm).await;
    let client = Client::new(test_app.session().clone());

    test_app
        .deploy_wasm_cell("spawner.wasm".to_owned(), SPAWNER_SRN.to_owned())
        .await;

    (test_app, client)
}

/// Retrieves the wasm content hash for a registered cell class.
async fn class_hash(client: &Client, class_name: &str) -> [u8; 32] {
    let info = assert_some!(
        assert_ok!(client.get_class_info(class_name).await),
        "class '{}' should exist",
        class_name
    );
    let BlobHash::Sha2(hash) = assert_some!(
        info.wasm_hash,
        "class '{}' should have a wasm hash",
        class_name
    );
    hash
}

/// Polls until the cell's placement presence matches `present`. Spawn and
/// terminate are fire-and-forget, so the placement updates asynchronously.
async fn wait_for_presence(client: &Client, sri: &Sri, present: bool) {
    for _ in 0..50 {
        if assert_ok!(client.placement_exists(sri).await) == present {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("cell {sri} did not reach present={present} in time");
}

/// Spawn a child cell using `#[init]` (no explicit state), then verify it is
/// present in the registry, records its parent, and publishes its seeded value.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn spawn_with_init() {
    // Arrange
    let (mut test_app, client) = setup().await;
    let child_hash = class_hash(&client, "spawn_child.wasm").await;
    // Subscribe before triggering so the child's published value isn't missed.
    let mut value_events = test_app.subscribe_cell_event(CHILD_VALUE_EVENT).await;

    // Act — command the spawner to spawn a child (no explicit state)
    let payload = assert_ok!(
        postcard::to_allocvec(&SpawnRequest {
            class: ClassRef::Hash(child_hash),
            local_name: Some(CHILD_LOCAL.into()),
            tags: None,
            arguments: None,
            detached: false,
            grace_ms: None,
            deadline_ms: None,
        }),
        "serialize spawn request"
    );
    test_app
        .command_send(SPAWNER_SRN, "spawn", Some(payload))
        .await;

    // Assert — child lands in the registry
    wait_for_presence(&client, &expected_child_sri(), true).await;

    // The child publishes its value on demand; observe it via the event.
    test_app
        .command_send(expected_child_sri().to_string(), "get_value", None)
        .await;
    let received = assert_ok!(value_events.receive().await);
    let value: i32 = assert_ok!(postcard::from_bytes(&received), "deser child value");
    assert_eq!(
        value, 0,
        "child initialized via #[init] should have default value"
    );

    // The child records the cell that spawned it.
    let child_info = assert_ok!(
        client.inspect_instance(&expected_child_sri()).await,
        "child instance should be inspectable"
    );
    assert_eq!(
        child_info.lineage.parent,
        Some(spawner_sri()),
        "spawned child should record its parent's SRI"
    );
}

/// Spawn a child cell by class name (instead of hash), then verify it is
/// present and publishes its seeded value.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn spawn_by_class_name() {
    // Arrange
    let (mut test_app, client) = setup().await;
    // Subscribe before triggering so the child's published value isn't missed.
    let mut value_events = test_app.subscribe_cell_event(CHILD_VALUE_EVENT).await;

    // Act — spawn by class name
    let payload = assert_ok!(
        postcard::to_allocvec(&SpawnRequest {
            class: ClassRef::Name("spawn_child.wasm".into()),
            local_name: Some(CHILD_LOCAL.into()),
            tags: None,
            arguments: None,
            detached: false,
            grace_ms: None,
            deadline_ms: None,
        }),
        "serialize spawn request"
    );
    test_app
        .command_send(SPAWNER_SRN, "spawn", Some(payload))
        .await;

    // Assert — child lands in the registry
    wait_for_presence(&client, &expected_child_sri(), true).await;

    // The child publishes its value on demand; observe it via the event.
    test_app
        .command_send(expected_child_sri().to_string(), "get_value", None)
        .await;
    let received = assert_ok!(value_events.receive().await);
    let value: i32 = assert_ok!(postcard::from_bytes(&received), "deser child value");
    assert_eq!(
        value, 0,
        "child initialized via #[init] should have default value"
    );
}

/// Spawn a child, then terminate it. Verify the child is no longer registered.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn terminate_deployed_cell() {
    // Arrange — spawn a child first
    let (test_app, client) = setup().await;
    let child_hash = class_hash(&client, "spawn_child.wasm").await;

    let spawn_payload = assert_ok!(
        postcard::to_allocvec(&SpawnRequest {
            class: ClassRef::Hash(child_hash),
            local_name: Some(CHILD_LOCAL.into()),
            tags: None,
            arguments: None,
            detached: false,
            grace_ms: None,
            deadline_ms: None,
        }),
        "serialize spawn request"
    );
    test_app
        .command_send(SPAWNER_SRN, "spawn", Some(spawn_payload))
        .await;
    wait_for_presence(&client, &expected_child_sri(), true).await;

    // Act — terminate the child (SRI string as a postcard-encoded payload)
    let terminate_payload = assert_ok!(
        postcard::to_allocvec(&expected_child_sri().to_string()),
        "serialize terminate request"
    );
    test_app
        .command_send(SPAWNER_SRN, "terminate", Some(terminate_payload))
        .await;

    // Assert — the child's placement disappears. Commands are fire-and-forget,
    // so the old `CellNotPresent` command-reply no longer applies; the terminate
    // is observed via the placement instead.
    wait_for_presence(&client, &expected_child_sri(), false).await;
    assert!(
        !assert_ok!(client.placement_exists(&expected_child_sri()).await),
        "terminated child should have no placement"
    );
}

// PARKED(new-model): in-cell errors are invisible under fire-and-forget; needs error-via-event redesign. Revisit.
/*
/// Spawning a cell with an SRI that is already in use should fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn spawn_duplicate_sri_fails() {
    // Arrange — spawn a child first
    let (test_app, client) = setup().await;
    let child_hash = class_hash(&client, "spawn_child.wasm").await;

    let payload = assert_ok!(
        postcard::to_allocvec(&SpawnRequest {
            class: ClassRef::Hash(child_hash),
            local_name: Some(CHILD_LOCAL.into(),
            tags: None,
            arguments: None,
            detached: false,
            grace_ms: None,
            deadline_ms: None,
        }),
        "serialize spawn request"
    );
    assert_ok!(
        test_app
            .command_send_wait_with_payload(spawner_sri(), "spawn", payload.clone())
            .await,
        "first spawn should succeed"
    );

    // Act — spawn again with the same SRI
    let outcome = test_app
        .command_send_wait_with_payload(spawner_sri(), "spawn", payload)
        .await;

    // Assert — the error mentions the duplicate SRI
    assert_matches!(
        outcome,
        Err(CellCommandError::CellError(msg)) if msg.contains("already exists"),
        "spawning with a duplicate SRI should fail with 'already exists'"
    );
}
*/

// PARKED(new-model): in-cell errors are invisible under fire-and-forget; needs error-via-event redesign. Revisit.
/*
/// Spawning with a class hash that doesn't match any registered class should fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn spawn_unknown_hash_fails() {
    // Arrange
    let (test_app, _client) = setup().await;

    // Act — spawn with a bogus hash
    let bogus_hash = [0xFFu8; 32];
    let payload = assert_ok!(
        postcard::to_allocvec(&SpawnRequest {
            class: ClassRef::Hash(bogus_hash),
            local_name: Some(CHILD_LOCAL.into(),
            tags: None,
            arguments: None,
            detached: false,
            grace_ms: None,
            deadline_ms: None,
        }),
        "serialize spawn request"
    );
    let outcome = test_app
        .command_send_wait_with_payload(spawner_sri(), "spawn", payload)
        .await;

    // Assert — the error mentions class not found
    assert_matches!(
        outcome,
        Err(CellCommandError::CellError(msg)) if msg.contains("class not found"),
        "spawning with an unknown class hash should fail with 'class not found'"
    );
}
*/

// PARKED(new-model): in-cell errors are invisible under fire-and-forget; needs error-via-event redesign. Revisit.
/*
/// Spawning with a class name that doesn't match any registered class should fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn spawn_unknown_name_fails() {
    // Arrange
    let (test_app, _client) = setup().await;

    // Act — spawn with a nonexistent class name
    let payload = assert_ok!(
        postcard::to_allocvec(&SpawnRequest {
            class: ClassRef::Name("nonexistent_class.wasm".into()),
            local_name: Some(CHILD_LOCAL.into(),
            tags: None,
            arguments: None,
            detached: false,
            grace_ms: None,
            deadline_ms: None,
        }),
        "serialize spawn request"
    );
    let outcome = test_app
        .command_send_wait_with_payload(spawner_sri(), "spawn", payload)
        .await;

    // Assert — the error mentions class not found
    assert_matches!(
        outcome,
        Err(CellCommandError::CellError(msg)) if msg.contains("class not found"),
        "spawning with an unknown class name should fail with 'class not found'"
    );
}
*/

// PARKED(new-model): in-cell errors are invisible under fire-and-forget; needs error-via-event redesign. Revisit.
/*
/// Terminating a cell that doesn't exist should fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn terminate_nonexistent_fails() {
    // Arrange
    let (test_app, _client) = setup().await;

    // Act — terminate a cell that was never spawned
    let payload = assert_ok!(
        postcard::to_allocvec("nonexistent_cell"),
        "serialize terminate request"
    );
    let outcome = test_app
        .command_send_wait_with_payload(spawner_sri(), "terminate", payload)
        .await;

    // Assert — the error mentions the cell not being found
    assert_matches!(
        outcome,
        Err(CellCommandError::CellError(msg)) if msg.contains("not found"),
        "terminating a nonexistent cell should fail with 'not found'"
    );
}
*/
