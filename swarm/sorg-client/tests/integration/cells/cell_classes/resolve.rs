use cell_protocol::ArtifactPlatform;
use claims::assert_ok;
use db_client::v1::Client as DbClient;

use super::{AOT_BINARY, CLASS_NAME, DUMMY_BINARY, class_blob, sorg_client};
use crate::integration::spawn_db_test_app;

/// Runtimes load a class's artifacts by its id: the linux exec the wasm, a
/// board the AOT binary.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn class_blobs_are_readable_by_id() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());
    let db = DbClient::new(test_app.session());

    // Arrange
    let wasm = assert_ok!(
        sorg.add_class(
            CLASS_NAME,
            DUMMY_BINARY.to_vec(),
            vec![(ArtifactPlatform::Riscv32imac, AOT_BINARY.to_vec())]
        )
        .await
    );
    let aot = cell_protocol::BlobHash::of(AOT_BINARY);

    // Act & Assert
    assert_eq!(class_blob(&db, wasm).await.as_deref(), Some(DUMMY_BINARY));
    assert_eq!(class_blob(&db, aot).await.as_deref(), Some(AOT_BINARY));
}
