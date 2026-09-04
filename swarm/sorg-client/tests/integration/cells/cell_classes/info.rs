use cell_protocol::{AddMode, ArtifactPlatform, BlobHash};
use claims::{assert_none, assert_ok, assert_some};

use super::{CLASS_NAME, DUMMY_BINARY, aot, sorg_client, wasm};
use crate::integration::spawn_db_test_app;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn wasm_only() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange
    assert_ok!(
        sorg.add_class_artifact(CLASS_NAME, wasm(DUMMY_BINARY), AddMode::Strict)
            .await
    );

    // Act
    let info = assert_some!(assert_ok!(sorg.get_class_info(CLASS_NAME).await));

    // Assert
    assert_eq!(info.name, CLASS_NAME);
    assert_eq!(info.wasm_hash, Some(BlobHash::of(DUMMY_BINARY)));
    assert!(info.artifacts.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn aot_only() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange
    let aot_blob: &[u8] = &[20, 21, 22];
    let meta_blob: &[u8] = &[30, 31, 32];
    assert_ok!(
        sorg.add_class_artifact(
            CLASS_NAME,
            aot(ArtifactPlatform::Riscv32imac, aot_blob, meta_blob),
            AddMode::Strict,
        )
        .await
    );

    // Act
    let info = assert_some!(assert_ok!(sorg.get_class_info(CLASS_NAME).await));

    // Assert
    assert_eq!(info.name, CLASS_NAME);
    assert!(info.wasm_hash.is_none());
    assert_eq!(1, info.artifacts.len());
    assert_eq!(ArtifactPlatform::Riscv32imac, info.artifacts[0].platform);
    assert_eq!(info.artifacts[0].aot_hash, BlobHash::of(aot_blob));
    assert_eq!(info.artifacts[0].meta_hash, BlobHash::of(meta_blob));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn full() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange — wasm + two aot targets
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
    let info = assert_some!(assert_ok!(sorg.get_class_info(CLASS_NAME).await));

    // Assert
    assert_eq!(info.name, CLASS_NAME);
    assert!(info.wasm_hash.is_some());
    assert_eq!(1, info.artifacts.len());
    assert!(
        info.artifacts
            .iter()
            .any(|a| a.platform == ArtifactPlatform::Riscv32imac)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn not_found() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Act
    let result = assert_ok!(sorg.get_class_info("nonexistent").await);

    // Assert
    assert_none!(result);
}
