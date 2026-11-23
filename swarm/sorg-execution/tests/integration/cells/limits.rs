//! Integration tests for the cell limits: a runaway handler is ended by the
//! fuel budget while a second cell keeps answering, a cell that declares more
//! memory than the node allows is not deployed, and growing memory past the
//! limit fails instead of ending the cell.

use std::{error::Error, time::Duration};

use sorg_client::EventQueue;
use sorg_tests::{register_fixture_class, swarm_config};
use tokio::time::timeout;

use crate::integration::spawn_test_app_with_swarm;

const CELL_CLASS: &str = "fuel_hog.wasm";
const HOG_SRI: &str = "hog_a";
const NEIGHBOUR_SRI: &str = "hog_b";

/// With a small fuel budget an endless handler must run out of fuel, and a
/// second cell must answer within 1 s while the first one is spinning.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn runaway_handler_is_aborted_and_neighbour_keeps_answering() -> Result<(), Box<dyn Error>>
{
    // Arrange - register the hog fixture and deploy it twice.
    let swarm = swarm_config!("cells/limits/limits.jsonnet");
    register_fixture_class(
        "../../tests/fixtures/cell-fuel-hog-logic",
        "fuel_hog",
        &swarm,
    )
    .await;
    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut ping_q = test_app.subscribe_cell_event("hog_ping").await;
    test_app.deploy_wasm_cell(CELL_CLASS, HOG_SRI).await;
    test_app.deploy_wasm_cell(CELL_CLASS, NEIGHBOUR_SRI).await;

    // Act - start the runaway handler, then ping the neighbour.
    test_app.command_send(HOG_SRI, "hog", None).await;
    let payload = serde_json::to_vec(&2i32)?;
    test_app
        .command_send(NEIGHBOUR_SRI, "ping", Some(payload))
        .await;

    // Assert - the neighbour answers within 1 s ...
    assert_eq!(receive_tag(&mut ping_q, Duration::from_secs(1)).await?, 2);

    // ... and the hog cell only reaches its own ping once the spinning run was
    // ended by the fuel budget.
    let payload = serde_json::to_vec(&1i32)?;
    test_app.command_send(HOG_SRI, "ping", Some(payload)).await;
    assert_eq!(receive_tag(&mut ping_q, Duration::from_secs(5)).await?, 1);

    Ok(())
}

/// The limits fixture starts with 2 pages and the node allows 6, so growing by
/// one page succeeds and reports the old size, while growing past the limit
/// reports `-1`.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn memory_grow_past_the_limit_fails() -> Result<(), Box<dyn Error>> {
    // Arrange
    let swarm = swarm_config!("cells/limits/limits.jsonnet");
    register_fixture_class(
        "../../tests/fixtures/cell-fuel-hog-logic",
        "fuel_hog",
        &swarm,
    )
    .await;
    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut grow_q = test_app.subscribe_cell_event("hog_grow").await;
    test_app.deploy_wasm_cell(CELL_CLASS, HOG_SRI).await;

    // Act
    test_app
        .command_send(HOG_SRI, "grow", Some(serde_json::to_vec(&1u32)?))
        .await;
    let within = receive_tag(&mut grow_q, Duration::from_secs(5)).await?;
    test_app
        .command_send(HOG_SRI, "grow", Some(serde_json::to_vec(&10u32)?))
        .await;
    let past = receive_tag(&mut grow_q, Duration::from_secs(5)).await?;

    // Assert
    assert_eq!(within, 2);
    assert_eq!(past, -1);

    Ok(())
}

/// The runtime-tags fixture declares 128 `KiB` of initial memory, so a node
/// limited to 64 `KiB` must refuse the deployment and name the limit.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn cell_declaring_more_memory_than_the_limit_is_not_deployed() {
    // Arrange - a node whose cell memory limit is below the fixture's memory.
    let swarm = swarm_config!("cells/limits/memory.jsonnet");
    register_fixture_class(
        "../../tests/fixtures/cell-runtime-tags-logic",
        "runtime_tags",
        &swarm,
    )
    .await;
    let test_app = spawn_test_app_with_swarm(swarm).await;

    // Act
    let err = test_app
        .try_deploy_wasm_cell("runtime_tags.wasm", "memory_limited")
        .await
        .expect_err("deploy must fail")
        .to_string();

    // Assert
    assert!(err.contains("cell_memory"), "{err}");
    assert!(err.contains("65536"), "{err}");
}

async fn receive_tag(queue: &mut EventQueue, limit: Duration) -> Result<i32, Box<dyn Error>> {
    let bytes = timeout(limit, queue.receive()).await??;

    Ok(serde_json::from_slice(&bytes)?)
}
