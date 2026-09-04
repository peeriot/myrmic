use cell_protocol::{AddMode, ArtifactLocation, ArtifactPlatform, Sri};
use claims::{assert_err, assert_none, assert_ok};
use db_client::v1::Client as DbClient;

use super::{
    CLASS_NAME, DUMMY_BINARY, INSTANCE_SRI, aot, blob_at_path, seed_instance, sorg_client, wasm,
};
use crate::integration::spawn_db_test_app;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn happy_path() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange — add a class
    assert_ok!(
        sorg.add_class_artifact(CLASS_NAME, wasm(DUMMY_BINARY), AddMode::Strict)
            .await
    );

    // Act — remove it
    assert_ok!(sorg.remove_class(CLASS_NAME).await);

    // Assert — registry is empty
    let classes = assert_ok!(sorg.list_classes().await);
    assert!(classes.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn not_found() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Act — remove a class that was never added
    assert_err!(sorg.remove_class("nonexistent").await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn with_all_artifacts() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());
    let db = DbClient::new(test_app.session());

    // Arrange — class with wasm + one aot targets
    assert_ok!(
        sorg.add_class_artifact(CLASS_NAME, wasm(DUMMY_BINARY), AddMode::Strict)
            .await
    );
    assert_ok!(
        sorg.add_class_artifact(
            CLASS_NAME,
            aot(ArtifactPlatform::Riscv32imac, &[40, 41, 42], &[50, 51, 52]),
            AddMode::Strict,
        )
        .await
    );

    // Act
    assert_ok!(sorg.remove_class(CLASS_NAME).await);

    // Assert — registry entry gone
    assert_none!(assert_ok!(sorg.get_class_info(CLASS_NAME).await));

    // Assert — all blobs gone
    assert!(
        blob_at_path(&db, ArtifactLocation::wasm(CLASS_NAME))
            .await
            .is_none()
    );
    assert!(
        blob_at_path(
            &db,
            ArtifactLocation::aot(CLASS_NAME, ArtifactPlatform::Riscv32imac)
        )
        .await
        .is_none()
    );
    assert!(
        blob_at_path(
            &db,
            ArtifactLocation::meta(CLASS_NAME, ArtifactPlatform::Riscv32imac)
        )
        .await
        .is_none()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn aot_only() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());
    let db = DbClient::new(test_app.session());

    // Arrange — class with only aot, no wasm
    assert_ok!(
        sorg.add_class_artifact(
            CLASS_NAME,
            aot(ArtifactPlatform::Riscv32imac, &[20, 21, 22], &[30, 31, 32]),
            AddMode::Strict,
        )
        .await
    );

    // Act
    assert_ok!(sorg.remove_class(CLASS_NAME).await);

    // Assert — registry entry gone, blobs gone
    let classes = assert_ok!(sorg.list_classes().await);
    assert!(classes.is_empty());
    assert!(
        blob_at_path(
            &db,
            ArtifactLocation::aot(CLASS_NAME, ArtifactPlatform::Riscv32imac)
        )
        .await
        .is_none()
    );
    assert!(
        blob_at_path(
            &db,
            ArtifactLocation::meta(CLASS_NAME, ArtifactPlatform::Riscv32imac)
        )
        .await
        .is_none()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn with_instances_is_rejected() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange — add a class and register an instance that references it
    assert_ok!(
        sorg.add_class_artifact(CLASS_NAME, wasm(DUMMY_BINARY), AddMode::Strict)
            .await
    );
    seed_instance(
        test_app.session(),
        &Sri::from_target(INSTANCE_SRI).unwrap(),
        CLASS_NAME,
    )
    .await;

    // Act — try to remove the class
    assert_err!(
        sorg.remove_class(CLASS_NAME).await,
        "remove_class should fail when instances exist"
    );

    // Assert — registry unchanged
    let classes = assert_ok!(sorg.list_classes().await, "list_classes should succeed");
    assert_eq!(
        1,
        classes.len(),
        "registry should still contain exactly one class"
    );
    assert_eq!(
        classes[0].name, CLASS_NAME,
        "class name should be unchanged"
    );
}
