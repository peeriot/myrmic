use cell_protocol::Sri;
use claims::assert_ok;
use sorg_client::Client;
use sorg_common::RequirementTags;
use sorg_tests::{build_and_register_cell_class, swarm_config};

use crate::integration::spawn_test_app_with_swarm;

const ROOM_SRI: &str = "room-cell-001";

/// Loading a cell via sorg-client should register it in the cell registry.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn deploy_cell_via_client() {
    // Arrange - build the room cell, start the swarm
    let swarm = swarm_config!("cells/sorg_client/swarm.jsonnet");
    build_and_register_cell_class("../../tests/fixtures/cell-room-logic", "room", &swarm).await;
    let test_app = spawn_test_app_with_swarm(swarm).await;
    let client = Client::new(test_app.session().clone());

    // Act - load the room cell via the sorg-client
    assert_ok!(
        client
            .deploy_wasm_cell(
                Sri::from_target(ROOM_SRI).unwrap(),
                "room.wasm",
                RequirementTags::default()
            )
            .await
    );

    // Assert - the room cell should have a placement
    let present = assert_ok!(
        client
            .placement_exists(&Sri::from_target(ROOM_SRI).unwrap())
            .await
    );
    assert!(present, "room cell should have a placement after loading");
}

/// Deleting a cell via sorg-client should remove its placement.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn undeploy_cell_via_client() {
    // Arrange - build, start, and deploy the room cell
    let swarm = swarm_config!("cells/sorg_client/swarm.jsonnet");
    build_and_register_cell_class("../../tests/fixtures/cell-room-logic", "room", &swarm).await;
    let test_app = spawn_test_app_with_swarm(swarm).await;
    let client = Client::new(test_app.session().clone());
    assert_ok!(
        client
            .deploy_wasm_cell(
                Sri::from_target(ROOM_SRI).unwrap(),
                "room.wasm",
                RequirementTags::default()
            )
            .await
    );
    assert!(
        assert_ok!(
            client
                .placement_exists(&Sri::from_target(ROOM_SRI).unwrap())
                .await
        ),
        "room cell should have a placement after loading"
    );

    // Act - delete the room cell
    assert_ok!(
        client
            .undeploy_cell(Sri::from_target(ROOM_SRI).unwrap())
            .await
    );

    // Assert - the room cell should no longer have a placement
    let present = assert_ok!(
        client
            .placement_exists(&Sri::from_target(ROOM_SRI).unwrap())
            .await
    );
    assert!(
        !present,
        "room cell should have no placement after deletion"
    );
}
