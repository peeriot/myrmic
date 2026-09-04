use cell_protocol::{AddMode, ArtifactLocation, ArtifactPlatform, BlobHash, Sri};
use claims::{assert_err, assert_ok};

use super::{
    CLASS_NAME, DUMMY_BINARY, INSTANCE_SRI, aot, blob_at_path, seed_instance, sorg_client, wasm,
};
use crate::integration::spawn_db_test_app;

const OTHER_BINARY: &[u8] = &[10, 11, 12, 13, 14, 15];

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn happy_path() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Act
    let class_info = assert_ok!(
        sorg.add_class_artifact(CLASS_NAME, wasm(DUMMY_BINARY), AddMode::Strict)
            .await
    );

    // Assert — class info has the right name and hash
    assert_eq!(class_info.name, CLASS_NAME);
    assert!(class_info.wasm_hash.is_some());

    // Assert — registry contains exactly this class
    let classes = assert_ok!(sorg.list_classes().await);
    assert_eq!(1, classes.len());
    assert_eq!(classes[0].name, CLASS_NAME);
    assert_eq!(classes[0].wasm_hash, class_info.wasm_hash);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn duplicate_name_is_rejected() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange — add the class
    let original = assert_ok!(
        sorg.add_class_artifact(CLASS_NAME, wasm(DUMMY_BINARY), AddMode::Strict)
            .await
    );

    // Act — add a different binary under the same name
    assert_err!(
        sorg.add_class_artifact(CLASS_NAME, wasm(OTHER_BINARY), AddMode::Strict)
            .await
    );

    // Assert — registry unchanged, still has original
    let classes = assert_ok!(sorg.list_classes().await);
    assert_eq!(1, classes.len());
    assert_eq!(classes[0].wasm_hash, original.wasm_hash);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn duplicate_hash_is_rejected() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange — add the class
    assert_ok!(
        sorg.add_class_artifact(CLASS_NAME, wasm(DUMMY_BINARY), AddMode::Strict)
            .await
    );

    // Act — add the same binary under a different name
    assert_err!(
        sorg.add_class_artifact("other-name", wasm(DUMMY_BINARY), AddMode::Strict)
            .await
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn duplicate_name_force() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange — add the class
    let original = assert_ok!(
        sorg.add_class_artifact(CLASS_NAME, wasm(DUMMY_BINARY), AddMode::Strict)
            .await
    );

    // Act — force-add a different binary under the same name
    let updated = assert_ok!(
        sorg.add_class_artifact(CLASS_NAME, wasm(OTHER_BINARY), AddMode::Force)
            .await
    );

    // Assert — registry has same name but new hash
    let classes = assert_ok!(sorg.list_classes().await);
    assert_eq!(1, classes.len());
    assert_eq!(classes[0].name, CLASS_NAME);
    assert_ne!(classes[0].wasm_hash, original.wasm_hash);
    assert_eq!(classes[0].wasm_hash, updated.wasm_hash);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn duplicate_hash_force() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange — add the class under the original name
    let original = assert_ok!(
        sorg.add_class_artifact(CLASS_NAME, wasm(DUMMY_BINARY), AddMode::Strict)
            .await
    );

    // Act — force-add the same binary under a different name
    let reassigned = assert_ok!(
        sorg.add_class_artifact("other-name", wasm(DUMMY_BINARY), AddMode::Force)
            .await
    );

    // Assert — registry has only the new name, same hash
    let classes = assert_ok!(sorg.list_classes().await);
    assert_eq!(1, classes.len());
    assert_eq!("other-name", classes[0].name);
    assert_eq!(original.wasm_hash, reassigned.wasm_hash);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn idempotent_add_is_accepted() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange — add the class
    let first = assert_ok!(
        sorg.add_class_artifact(CLASS_NAME, wasm(DUMMY_BINARY), AddMode::Strict)
            .await
    );

    // Act — add the exact same class again (same name, same binary)
    let second = assert_ok!(
        sorg.add_class_artifact(CLASS_NAME, wasm(DUMMY_BINARY), AddMode::Strict)
            .await
    );

    // Assert — returns the same info, registry unchanged
    assert_eq!(first.name, second.name);
    assert_eq!(first.wasm_hash, second.wasm_hash);

    let classes = assert_ok!(sorg.list_classes().await);
    assert_eq!(1, classes.len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn name_and_hash_conflict_with_different_entries_is_rejected() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange — "test-cell" has OTHER_BINARY, "other-name" has DUMMY_BINARY
    let first = assert_ok!(
        sorg.add_class_artifact(CLASS_NAME, wasm(OTHER_BINARY), AddMode::Strict)
            .await
    );
    let second = assert_ok!(
        sorg.add_class_artifact("other-name", wasm(DUMMY_BINARY), AddMode::Strict)
            .await
    );

    // Act — force-add "test-cell" with DUMMY_BINARY: name conflicts with first,
    // hash conflicts with second — two different entries
    assert_err!(
        sorg.add_class_artifact(CLASS_NAME, wasm(DUMMY_BINARY), AddMode::Force)
            .await
    );

    // Assert — both original entries are untouched
    let classes = assert_ok!(sorg.list_classes().await);
    assert_eq!(2, classes.len());
    assert!(
        classes
            .iter()
            .any(|c| c.name == CLASS_NAME && c.wasm_hash == first.wasm_hash)
    );
    assert!(
        classes
            .iter()
            .any(|c| c.name == "other-name" && c.wasm_hash == second.wasm_hash)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn add_wasm_to_existing_aot_class() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange — create a class with only an aot artifact
    let aot_info = assert_ok!(
        sorg.add_class_artifact(
            CLASS_NAME,
            aot(ArtifactPlatform::Riscv32imac, &[20, 21, 22], &[30, 31, 32]),
            AddMode::Strict,
        )
        .await
    );
    assert!(aot_info.wasm_hash.is_none());
    assert_eq!(1, aot_info.artifacts.len());

    // Act — add a wasm binary to the same class
    let updated = assert_ok!(
        sorg.add_class_artifact(CLASS_NAME, wasm(DUMMY_BINARY), AddMode::Strict)
            .await
    );

    // Assert — wasm_hash matches expected, aot artifact is preserved
    assert_eq!(updated.wasm_hash, Some(BlobHash::of(DUMMY_BINARY)));
    assert_eq!(1, updated.artifacts.len());
    assert_eq!(ArtifactPlatform::Riscv32imac, updated.artifacts[0].platform);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn add_aot_to_existing_wasm_class() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange — create a class with wasm
    let wasm_info = assert_ok!(
        sorg.add_class_artifact(CLASS_NAME, wasm(DUMMY_BINARY), AddMode::Strict)
            .await
    );
    assert!(wasm_info.wasm_hash.is_some());
    assert!(wasm_info.artifacts.is_empty());

    // Act — add an aot artifact
    let updated = assert_ok!(
        sorg.add_class_artifact(
            CLASS_NAME,
            aot(ArtifactPlatform::Riscv32imac, &[20, 21, 22], &[30, 31, 32]),
            AddMode::Strict,
        )
        .await
    );

    // Assert — wasm_hash preserved, artifact added
    assert_eq!(updated.wasm_hash, wasm_info.wasm_hash);
    assert_eq!(1, updated.artifacts.len());
    assert_eq!(ArtifactPlatform::Riscv32imac, updated.artifacts[0].platform);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn add_aot_creates_class() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    let aot_blob: &[u8] = &[20, 21, 22];
    let meta_blob: &[u8] = &[30, 31, 32];

    // Act
    let info = assert_ok!(
        sorg.add_class_artifact(
            CLASS_NAME,
            aot(ArtifactPlatform::Riscv32imac, aot_blob, meta_blob),
            AddMode::Strict,
        )
        .await
    );

    // Assert — class created with no wasm, one artifact entry
    assert_eq!(info.name, CLASS_NAME);
    assert!(info.wasm_hash.is_none());
    assert_eq!(1, info.artifacts.len());
    assert_eq!(ArtifactPlatform::Riscv32imac, info.artifacts[0].platform);

    assert_eq!(info.artifacts[0].aot_hash, BlobHash::of(aot_blob));
    assert_eq!(info.artifacts[0].meta_hash, BlobHash::of(meta_blob));

    // Assert — registry contains exactly this class
    let classes = assert_ok!(sorg.list_classes().await);
    assert_eq!(1, classes.len());
    assert_eq!(classes[0].name, CLASS_NAME);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn add_aot_conflict_force() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange — add an aot artifact
    let original = assert_ok!(
        sorg.add_class_artifact(
            CLASS_NAME,
            aot(ArtifactPlatform::Riscv32imac, &[20, 21, 22], &[30, 31, 32]),
            AddMode::Strict,
        )
        .await
    );

    // Act — force-add different blobs for the same target
    let new_aot: &[u8] = &[99, 98, 97];
    let new_meta: &[u8] = &[89, 88, 87];
    let updated = assert_ok!(
        sorg.add_class_artifact(
            CLASS_NAME,
            aot(ArtifactPlatform::Riscv32imac, new_aot, new_meta),
            AddMode::Force,
        )
        .await
    );

    // Assert — artifact overwritten with new hashes
    assert_eq!(1, updated.artifacts.len());
    assert_ne!(
        original.artifacts[0].aot_hash,
        updated.artifacts[0].aot_hash
    );
    assert_ne!(
        original.artifacts[0].meta_hash,
        updated.artifacts[0].meta_hash
    );

    assert_eq!(updated.artifacts[0].aot_hash, BlobHash::of(new_aot));
    assert_eq!(updated.artifacts[0].meta_hash, BlobHash::of(new_meta));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn add_aot_conflict_rejected() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange — add an aot artifact
    let original = assert_ok!(
        sorg.add_class_artifact(
            CLASS_NAME,
            aot(ArtifactPlatform::Riscv32imac, &[20, 21, 22], &[30, 31, 32]),
            AddMode::Strict,
        )
        .await
    );

    // Act — add different blobs for the same target, strict mode
    assert_err!(
        sorg.add_class_artifact(
            CLASS_NAME,
            aot(ArtifactPlatform::Riscv32imac, &[99, 98, 97], &[89, 88, 87]),
            AddMode::Strict,
        )
        .await
    );

    // Assert — original artifact unchanged
    let classes = assert_ok!(sorg.list_classes().await);
    assert_eq!(1, classes.len());
    assert_eq!(1, classes[0].artifacts.len());
    assert_eq!(
        original.artifacts[0].aot_hash,
        classes[0].artifacts[0].aot_hash
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn add_aot_idempotent() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    let aot_blob: &[u8] = &[20, 21, 22];
    let meta_blob: &[u8] = &[30, 31, 32];

    // Arrange — add an aot artifact
    let first = assert_ok!(
        sorg.add_class_artifact(
            CLASS_NAME,
            aot(ArtifactPlatform::Riscv32imac, aot_blob, meta_blob),
            AddMode::Strict,
        )
        .await
    );

    // Act — add the exact same aot artifact again
    let second = assert_ok!(
        sorg.add_class_artifact(
            CLASS_NAME,
            aot(ArtifactPlatform::Riscv32imac, aot_blob, meta_blob),
            AddMode::Strict,
        )
        .await
    );

    // Assert — same info, single artifact entry
    assert_eq!(1, second.artifacts.len());
    assert_eq!(first.artifacts[0].aot_hash, second.artifacts[0].aot_hash);
    assert_eq!(first.artifacts[0].meta_hash, second.artifacts[0].meta_hash);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn add_wasm_hash_conflict_on_aot_only_class_rejected() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange — class A has wasm, class B has only aot
    assert_ok!(
        sorg.add_class_artifact("class-a", wasm(DUMMY_BINARY), AddMode::Strict)
            .await
    );
    assert_ok!(
        sorg.add_class_artifact(
            "class-b",
            aot(ArtifactPlatform::Riscv32imac, &[20, 21, 22], &[30, 31, 32]),
            AddMode::Strict,
        )
        .await
    );

    // Act — add the same wasm to class B (hash conflict with class A), strict
    assert_err!(
        sorg.add_class_artifact("class-b", wasm(DUMMY_BINARY), AddMode::Strict)
            .await
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn add_wasm_hash_conflict_on_aot_only_class_force() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange — class A has wasm, class B has only aot
    assert_ok!(
        sorg.add_class_artifact("class-a", wasm(DUMMY_BINARY), AddMode::Strict)
            .await
    );
    assert_ok!(
        sorg.add_class_artifact(
            "class-b",
            aot(ArtifactPlatform::Riscv32imac, &[20, 21, 22], &[30, 31, 32]),
            AddMode::Strict,
        )
        .await
    );

    // Act — force-add the same wasm to class B (reassigns from class A)
    let updated = assert_ok!(
        sorg.add_class_artifact("class-b", wasm(DUMMY_BINARY), AddMode::Force)
            .await
    );

    // Assert — class B now has wasm + its original aot artifact
    assert!(updated.wasm_hash.is_some());
    assert_eq!(1, updated.artifacts.len());
    assert_eq!(ArtifactPlatform::Riscv32imac, updated.artifacts[0].platform);

    // Assert — class A is gone (wasm reassigned, no artifacts left)
    let classes = assert_ok!(sorg.list_classes().await);
    assert_eq!(1, classes.len());
    assert_eq!("class-b", classes[0].name);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn force_reassign_wasm_cleans_up_source_aot() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());
    let db = db_client::v1::Client::new(test_app.session());

    // Arrange — class A has wasm + aot
    assert_ok!(
        sorg.add_class_artifact("class-a", wasm(DUMMY_BINARY), AddMode::Strict)
            .await
    );
    assert_ok!(
        sorg.add_class_artifact(
            "class-a",
            aot(ArtifactPlatform::Riscv32imac, &[20, 21, 22], &[30, 31, 32]),
            AddMode::Strict,
        )
        .await
    );

    // Act — force-reassign the wasm to class B
    assert_ok!(
        sorg.add_class_artifact("class-b", wasm(DUMMY_BINARY), AddMode::Force)
            .await
    );

    // Assert — class A's aot blobs are cleaned up (not orphaned)
    assert!(
        blob_at_path(&db, ArtifactLocation::wasm("class-a"))
            .await
            .is_none()
    );
    assert!(
        blob_at_path(
            &db,
            ArtifactLocation::aot("class-a", ArtifactPlatform::Riscv32imac)
        )
        .await
        .is_none()
    );
    assert!(
        blob_at_path(
            &db,
            ArtifactLocation::meta("class-a", ArtifactPlatform::Riscv32imac)
        )
        .await
        .is_none()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn with_instances_is_rejected_even_with_force() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange — add the class and register an instance that references it
    let original = assert_ok!(
        sorg.add_class_artifact(CLASS_NAME, wasm(DUMMY_BINARY), AddMode::Strict)
            .await
    );
    seed_instance(
        test_app.session(),
        &Sri::from_target(INSTANCE_SRI).unwrap(),
        CLASS_NAME,
    )
    .await;

    // Act — force-add a different binary
    assert_err!(
        sorg.add_class_artifact(CLASS_NAME, wasm(OTHER_BINARY), AddMode::Force)
            .await,
        "force add_class_artifact should fail when instances exist"
    );

    // Assert — registry unchanged
    let classes = assert_ok!(sorg.list_classes().await, "list_classes should succeed");
    assert_eq!(
        1,
        classes.len(),
        "registry should still contain exactly one class"
    );
    assert_eq!(
        classes[0].wasm_hash, original.wasm_hash,
        "class hash should be unchanged"
    );
}
