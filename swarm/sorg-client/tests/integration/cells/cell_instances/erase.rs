use claims::{assert_err, assert_ok};

use cell_protocol::Sri;
use sorg_common::RequirementTags;

use super::{CLASS_NAME, INSTANCE_SRI, seed_instance, sorg_client};
use crate::integration::spawn_db_test_app;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn happy_path() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());
    let sri = Sri::from_target(INSTANCE_SRI).unwrap();

    // Arrange — seed an instance
    seed_instance(test_app.session(), &sri, CLASS_NAME).await;

    // Act
    assert_ok!(
        sorg.erase_instance(&sri).await,
        "erase_instance should succeed"
    );

    // Assert — instance no longer appears in list
    let instances = assert_ok!(sorg.list_instances().await, "list_instances should succeed");
    assert!(
        instances.is_empty(),
        "instance list should be empty after erasure"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn deployed_is_rejected() {
    let swarm = sorg_tests::swarm_config!("full.jsonnet");
    let cell_class = "dummy";
    let cell_sri = "test-deployed-erase";
    sorg_tests::build_and_register_cell_class(
        "../../tests/fixtures/dummy_cell",
        cell_class,
        &swarm,
    )
    .await;
    let test_app = crate::integration::spawn_full_test_app_with_swarm(swarm).await;
    let sorg = sorg_client(test_app.session());
    let sri = Sri::from_target(cell_sri).unwrap();
    let class_name = format!("{cell_class}.wasm");

    // Arrange — deploy the cell (this registers the instance)
    assert_ok!(
        sorg.deploy_wasm_cell(sri, &class_name, RequirementTags::default())
            .await,
        "deploy should succeed"
    );

    // Act — try to erase the deployed instance
    assert_err!(
        sorg.erase_instance(&sri).await,
        "erase_instance for deployed instance should fail"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn not_found() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());
    let sri = Sri::from_target(INSTANCE_SRI).unwrap();

    // Act — erase an SRI that was never created
    assert_err!(
        sorg.erase_instance(&sri).await,
        "erase_instance for missing instance should fail"
    );
}
