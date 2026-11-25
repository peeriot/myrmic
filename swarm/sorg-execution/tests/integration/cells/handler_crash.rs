//! A panic in a child's command handler crashes the child, and its parent's
//! monitor handler is told it `Crashed`.

use std::time::Duration;

use cell_protocol::Sri;
use claims::assert_ok;
use myrmic_common::cells::{ClassRef, LostReason, SpawnRequest};
use sorg_client::Client;
use sorg_tests::{TestApp, register_fixture_class, swarm_config};

use super::spawning::wait_for_presence;
use crate::integration::spawn_test_app_with_swarm;

const PARENT_SRN: &str = "crash_parent_cell";
const CHILD_LOCAL: &str = "crashing_child";
/// `Kv::new("crash_parent")` stores `"crash_parent/<key>"`.
const LOST_REASON_KEY: &str = "crash_parent/lost";

fn child_sri() -> Sri {
    let parent = assert_ok!(Sri::of_path(PARENT_SRN));
    assert_ok!(parent.child(CHILD_LOCAL))
}

/// The reason the parent's monitor handler stored, once it has.
async fn lost_reason(app: &TestApp) -> Option<LostReason> {
    for _ in 0..100 {
        if let Some(raw) = app.read_cell_kv(PARENT_SRN, LOST_REASON_KEY).await {
            return Some(assert_ok!(
                postcard::from_bytes(&raw),
                "decode the stored lost reason"
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn child_handler_panic_reports_crashed_to_parent() {
    // Arrange - the parent deployed, the crash cell spawned as its child
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    register_fixture_class("../../tests/fixtures/cell-crash-logic", "crash", &swarm).await;
    register_fixture_class(
        "../../tests/fixtures/cell-crash-parent",
        "crash_parent",
        &swarm,
    )
    .await;
    let test_app = spawn_test_app_with_swarm(swarm).await;
    let client = Client::new(test_app.session().clone());
    test_app
        .deploy_wasm_cell("crash_parent.wasm", PARENT_SRN)
        .await;
    let payload = assert_ok!(
        postcard::to_allocvec(&SpawnRequest {
            class: ClassRef::Name("crash.wasm".into()),
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
        .command_send(PARENT_SRN, "spawn", Some(payload))
        .await;
    wait_for_presence(&client, &child_sri(), true).await;

    // Act - a command whose handler panics
    test_app
        .command_send(child_sri().to_string(), "panic_always", None)
        .await;

    // Assert - the parent was told its child crashed
    assert_eq!(
        lost_reason(&test_app).await,
        Some(LostReason::Crashed),
        "the parent's monitor handler was not told the child crashed"
    );
}
