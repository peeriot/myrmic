//! Integration test for the `runtime` host functions: a cell reading the host
//! runtime's id and effective tag set.

use claims::assert_ok;
use sorg_tests::{build_and_register_cell_class, swarm_config};

use crate::integration::spawn_test_app_with_swarm;

const CELL_CLASS: &str = "runtime_tags.wasm";
const CELL_SRI: &str = "runtime_tags_test";

/// A cell deployed on a Linux runtime should read that runtime's id (non-empty)
/// and its effective tags (including the intrinsic `linux` tag) through the
/// `runtime` host functions.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn cell_reads_runtime_id_and_tags() {
    // Arrange — build the runtime-tags cell, start a Linux swarm, subscribe to
    // the event it reports on.
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class(
        "../../tests/fixtures/cell-runtime-tags-logic",
        "runtime_tags",
        &swarm,
    )
    .await;
    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut report_q = test_app.subscribe_cell_event("runtime_report").await;

    // Act — deploy the cell and ask it to report what it sees of its runtime.
    test_app.deploy_wasm_cell(CELL_CLASS, CELL_SRI).await;
    test_app
        .command_send(CELL_SRI, "report_runtime", None)
        .await;

    // Assert — payload is "<id>|<tag>,<tag>,…"; the id is non-empty and the
    // effective tags carry the intrinsic `linux` tag.
    let received = assert_ok!(report_q.receive().await);
    let text = String::from_utf8(received).expect("runtime report should be utf8");
    let (id, tags) = text
        .split_once('|')
        .expect("payload should be '<id>|<tags>'");
    assert!(!id.is_empty(), "runtime id should be non-empty");
    let tags: Vec<&str> = tags.split(',').collect();
    assert!(
        tags.contains(&"linux"),
        "effective tags should contain the intrinsic `linux` tag, got: {tags:?}"
    );
}
