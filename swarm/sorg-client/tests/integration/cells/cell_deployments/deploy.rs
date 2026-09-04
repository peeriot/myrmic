use cell_protocol::Sri;
use claims::{assert_err, assert_ok};
use sorg_common::RequirementTags;

use super::{CELL_CLASS, CELL_SRI, output_marker_seen, spawn_test_app_with_dummy_cell};

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn happy_path() {
    let (test_app, sorg) = spawn_test_app_with_dummy_cell().await;

    let sri = Sri::from_target(CELL_SRI).unwrap();
    let class_name = format!("{CELL_CLASS}.wasm");

    // Act — deploy by SRI + class; the deploy registers the instance, so there is no
    // separate create_instance step.
    assert_ok!(
        sorg.deploy_wasm_cell(sri, &class_name, RequirementTags::default())
            .await,
        "deploy should succeed"
    );

    // Assert — placed, and actually running: a fire-and-forget `output` command
    // reaches the cell and its DB write becomes observable.
    assert!(
        assert_ok!(sorg.placement_exists(&sri).await),
        "cell should have a placement"
    );
    test_app.command_send(CELL_SRI, "output", None).await;
    assert!(
        output_marker_seen(&test_app, CELL_SRI).await,
        "deployed cell should process the output command (marker written)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn unregistered_class_is_rejected() {
    let (_test_app, sorg) = spawn_test_app_with_dummy_cell().await;

    let sri = Sri::from_target("test-unregistered-class").unwrap();

    // Act — deploy a class that was never registered
    let result = sorg
        .deploy_wasm_cell(sri, "nonexistent.wasm", RequirementTags::default())
        .await;

    // Assert
    assert_err!(result, "deploying an unregistered class should fail");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn duplicate_deploy_is_rejected() {
    let (_test_app, sorg) = spawn_test_app_with_dummy_cell().await;

    let sri = Sri::from_target(CELL_SRI).unwrap();
    let class_name = format!("{CELL_CLASS}.wasm");

    // Arrange — deploy once (this also registers the instance)
    assert_ok!(
        sorg.deploy_wasm_cell(sri, &class_name, RequirementTags::default())
            .await,
        "first deploy should succeed"
    );

    // Act — deploy the same SRI again
    let result = sorg
        .deploy_wasm_cell(sri, &class_name, RequirementTags::default())
        .await;

    // Assert
    assert_err!(result, "deploying an already-deployed cell should fail");
}
