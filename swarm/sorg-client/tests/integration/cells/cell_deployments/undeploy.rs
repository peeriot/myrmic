use cell_protocol::Sri;
use claims::assert_ok;
use sorg_common::RequirementTags;

use super::{CELL_CLASS, CELL_SRI, output_marker_seen, spawn_test_app_with_dummy_cell};

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn deploy_then_undeploy() {
    let (test_app, sorg) = spawn_test_app_with_dummy_cell().await;

    let sri = Sri::from_target(CELL_SRI).unwrap();
    let class_name = format!("{CELL_CLASS}.wasm");

    // Arrange — deploy (this also registers the instance)
    assert_ok!(
        sorg.deploy_wasm_cell(sri, &class_name, RequirementTags::default())
            .await,
        "deploy_cell should succeed"
    );

    // Sanity check — cell is running: a fire-and-forget `output` command lands
    test_app.command_send(CELL_SRI, "output", None).await;
    assert!(
        output_marker_seen(&test_app, CELL_SRI).await,
        "deployed cell should process commands before undeploy"
    );

    // Act — undeploy
    assert_ok!(
        sorg.undeploy_cell(sri).await,
        "undeploy_cell should succeed"
    );

    // Assert — cell no longer has a placement
    assert!(
        !assert_ok!(sorg.placement_exists(&sri).await),
        "cell should no longer have a placement"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn undeploy_erases_instance_row() {
    let (_test_app, sorg) = spawn_test_app_with_dummy_cell().await;

    let sri = Sri::from_target(CELL_SRI).unwrap();
    let class_name = format!("{CELL_CLASS}.wasm");

    // Arrange — deploy (registers the instance), then undeploy
    assert_ok!(
        sorg.deploy_wasm_cell(sri, &class_name, RequirementTags::default())
            .await,
        "deploy should succeed"
    );
    assert_ok!(sorg.undeploy_cell(sri).await, "undeploy should succeed");

    // Act — the follow-up sweep teardown does: finds nothing, tolerates it
    let erased = assert_ok!(
        sorg.erase_instance_if_present(&sri).await,
        "redundant erase should be tolerated after undeploy"
    );

    // Assert — undeploy itself erased the row, and none remain
    assert!(
        !erased,
        "undeploy should have already erased the instance row"
    );
    let instances = assert_ok!(sorg.list_instances().await, "list_instances should succeed");
    assert!(
        instances.is_empty(),
        "instance list should be empty after undeploy"
    );
}
