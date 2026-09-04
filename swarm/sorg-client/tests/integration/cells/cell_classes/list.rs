use cell_protocol::AddMode;
use claims::assert_ok;

use super::{sorg_client, wasm};
use crate::integration::spawn_db_test_app;

const CLASS_A: &str = "cell-a";
const CLASS_B: &str = "cell-b";
const BINARY_A: &[u8] = &[0, 1, 2, 3];
const BINARY_B: &[u8] = &[10, 11, 12, 13];

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn empty() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Act
    let classes = assert_ok!(sorg.list_classes().await);

    // Assert
    assert!(classes.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn returns_added_classes() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange — add two classes
    let info_a = assert_ok!(
        sorg.add_class_artifact(CLASS_A, wasm(BINARY_A), AddMode::Strict)
            .await
    );
    let info_b = assert_ok!(
        sorg.add_class_artifact(CLASS_B, wasm(BINARY_B), AddMode::Strict)
            .await
    );

    // Act
    let classes = assert_ok!(sorg.list_classes().await);

    // Assert — both present with correct names and hashes
    assert_eq!(classes.len(), 2);
    assert!(
        classes
            .iter()
            .any(|c| c.name == CLASS_A && c.wasm_hash == info_a.wasm_hash)
    );
    assert!(
        classes
            .iter()
            .any(|c| c.name == CLASS_B && c.wasm_hash == info_b.wasm_hash)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn reflects_removal() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange — add two classes, remove one
    assert_ok!(
        sorg.add_class_artifact(CLASS_A, wasm(BINARY_A), AddMode::Strict)
            .await
    );
    let info_b = assert_ok!(
        sorg.add_class_artifact(CLASS_B, wasm(BINARY_B), AddMode::Strict)
            .await
    );
    assert_ok!(sorg.remove_class(CLASS_A).await);

    // Act
    let classes = assert_ok!(sorg.list_classes().await);

    // Assert — only the remaining class
    assert_eq!(classes.len(), 1);
    assert_eq!(classes[0].name, CLASS_B);
    assert_eq!(classes[0].wasm_hash, info_b.wasm_hash);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn unchanged_after_rejected_add() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange — add a class
    let info = assert_ok!(
        sorg.add_class_artifact(CLASS_A, wasm(BINARY_A), AddMode::Strict)
            .await
    );

    // Act — attempt duplicate name (rejected)
    let _ = sorg
        .add_class_artifact(CLASS_A, wasm(BINARY_B), AddMode::Strict)
        .await;

    // Assert — list unchanged
    let classes = assert_ok!(sorg.list_classes().await);
    assert_eq!(classes.len(), 1);
    assert_eq!(classes[0].name, CLASS_A);
    assert_eq!(classes[0].wasm_hash, info.wasm_hash);
}
