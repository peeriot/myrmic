use cell_protocol::{AddMode, ArtifactLocation, ArtifactPlatform};
use claims::{assert_ok, assert_some};
use db_client::v1::Client as DbClient;

use super::{CLASS_NAME, DUMMY_BINARY, aot, blob_at_path, sorg_client, wasm};
use crate::integration::spawn_db_test_app;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn wasm_by_path() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());
    let db = DbClient::new(test_app.session());

    // Arrange — store a wasm artifact via the registry
    assert_ok!(
        sorg.add_class_artifact(CLASS_NAME, wasm(DUMMY_BINARY), AddMode::Strict)
            .await
    );

    // Act + Assert — resolved bytes match the original binary
    let blob = assert_some!(blob_at_path(&db, ArtifactLocation::wasm(CLASS_NAME)).await);
    assert_eq!(blob, DUMMY_BINARY);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn aot_meta_by_path() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());
    let db = DbClient::new(test_app.session());

    let aot_bytes: &[u8] = &[20, 21, 22];
    let meta_bytes: &[u8] = &[30, 31, 32];

    // Arrange — store an aot artifact via the registry
    assert_ok!(
        sorg.add_class_artifact(
            CLASS_NAME,
            aot(ArtifactPlatform::Riscv32imac, aot_bytes, meta_bytes),
            AddMode::Strict,
        )
        .await
    );

    // Act + Assert — resolved bytes match originals
    let resolved_aot = assert_some!(
        blob_at_path(
            &db,
            ArtifactLocation::aot(CLASS_NAME, ArtifactPlatform::Riscv32imac)
        )
        .await
    );
    assert_eq!(resolved_aot, aot_bytes);

    let resolved_meta = assert_some!(
        blob_at_path(
            &db,
            ArtifactLocation::meta(CLASS_NAME, ArtifactPlatform::Riscv32imac)
        )
        .await
    );
    assert_eq!(resolved_meta, meta_bytes);
}
