use std::collections::BTreeMap;

use cell_protocol::{ArtifactPlatform, BlobHash, ClassInfo};
use claims::{assert_ok, assert_some};

use super::{AOT_BINARY, CLASS_NAME, DUMMY_BINARY, OTHER_BINARY, OTHER_NAME, sorg_client};
use crate::integration::spawn_db_test_app;

const PLATFORM: ArtifactPlatform = ArtifactPlatform::Riscv32imac;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn wasm_class_is_identified_by_its_content() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Act
    let id = assert_ok!(
        sorg.add_class(CLASS_NAME, DUMMY_BINARY.to_vec(), vec![])
            .await
    );

    // Assert — the id is the wasm's hash, the name points to it
    assert_eq!(id, BlobHash::of(DUMMY_BINARY));
    assert_eq!(
        assert_ok!(sorg.resolve_class_name(CLASS_NAME).await),
        Some(id)
    );
    assert_eq!(
        assert_ok!(sorg.get_class_info(id).await),
        Some(ClassInfo::Wasm {
            aot: BTreeMap::new(),
            spawns: vec![],
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn adding_the_same_wasm_again_is_idempotent() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange
    let first = assert_ok!(
        sorg.add_class(CLASS_NAME, DUMMY_BINARY.to_vec(), vec![])
            .await
    );

    // Act
    let second = assert_ok!(
        sorg.add_class(CLASS_NAME, DUMMY_BINARY.to_vec(), vec![])
            .await
    );

    // Assert — one class
    assert_eq!(first, second);
    assert_eq!(1, assert_ok!(sorg.list_classes().await).len());
}

/// The same crate deployed under two names: both names label the one class, and
/// neither deploy takes it away from the other.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn several_names_point_to_one_class() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange
    let id = assert_ok!(
        sorg.add_class(CLASS_NAME, DUMMY_BINARY.to_vec(), vec![])
            .await
    );

    // Act
    assert_ok!(
        sorg.add_class(OTHER_NAME, DUMMY_BINARY.to_vec(), vec![])
            .await
    );

    // Assert
    for name in [CLASS_NAME, OTHER_NAME] {
        assert_eq!(
            assert_ok!(sorg.resolve_class_name(name).await),
            Some(id),
            "'{name}' should point to the shared class"
        );
    }
    assert_some!(assert_ok!(sorg.get_class_info(id).await));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn binding_a_name_to_another_class_moves_only_the_name() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange
    let old = assert_ok!(
        sorg.add_class(CLASS_NAME, DUMMY_BINARY.to_vec(), vec![])
            .await
    );

    // Act
    let new = assert_ok!(
        sorg.add_class(CLASS_NAME, OTHER_BINARY.to_vec(), vec![])
            .await
    );

    // Assert — the name moved, the old class is still there
    assert_eq!(
        assert_ok!(sorg.resolve_class_name(CLASS_NAME).await),
        Some(new)
    );
    assert_some!(assert_ok!(sorg.get_class_info(old).await));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn aot_is_added_to_the_wasm_class_the_name_points_to() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange
    let wasm = assert_ok!(
        sorg.add_class(CLASS_NAME, DUMMY_BINARY.to_vec(), vec![])
            .await
    );

    // Act
    let aot = assert_ok!(
        sorg.add_class_aot(CLASS_NAME, PLATFORM, AOT_BINARY.to_vec())
            .await
    );

    // Assert — an AOT entry of its own, pointed to by the wasm class
    assert_eq!(aot, BlobHash::of(AOT_BINARY));
    assert_eq!(
        assert_ok!(sorg.get_class_info(aot).await),
        Some(ClassInfo::Aot {
            platform: PLATFORM,
            crc: wasm_storage::metadata::CRC.checksum(AOT_BINARY),
        })
    );
    assert_eq!(
        assert_ok!(sorg.get_class_info(wasm).await),
        Some(ClassInfo::Wasm {
            aot: BTreeMap::from([(PLATFORM, aot)]),
            spawns: vec![],
        })
    );
    assert_eq!(
        assert_ok!(sorg.resolve_class_name(CLASS_NAME).await),
        Some(wasm),
        "the name should still point to the wasm class"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn aot_without_a_wasm_class_is_a_class_of_its_own() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Act
    let aot = assert_ok!(
        sorg.add_class_aot(CLASS_NAME, PLATFORM, AOT_BINARY.to_vec())
            .await
    );

    // Assert
    assert_eq!(
        assert_ok!(sorg.resolve_class_name(CLASS_NAME).await),
        Some(aot)
    );
    assert_eq!(
        assert_ok!(sorg.get_class_info(aot).await),
        Some(ClassInfo::Aot {
            platform: PLATFORM,
            crc: wasm_storage::metadata::CRC.checksum(AOT_BINARY),
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn wasm_classes_share_an_identical_aot() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Act — two wasm classes compiled to the same AOT binary
    let first = assert_ok!(
        sorg.add_class(
            CLASS_NAME,
            DUMMY_BINARY.to_vec(),
            vec![(PLATFORM, AOT_BINARY.to_vec())]
        )
        .await
    );
    let second = assert_ok!(
        sorg.add_class(
            OTHER_NAME,
            OTHER_BINARY.to_vec(),
            vec![(PLATFORM, AOT_BINARY.to_vec())]
        )
        .await
    );

    // Assert — both point to the one AOT entry
    let aot = BlobHash::of(AOT_BINARY);
    for class in [first, second] {
        let Some(ClassInfo::Wasm { aot: compiled, .. }) =
            assert_ok!(sorg.get_class_info(class).await)
        else {
            panic!("class {} should be a wasm class", class.to_hex());
        };
        assert_eq!(compiled, BTreeMap::from([(PLATFORM, aot)]));
    }
    assert_eq!(3, assert_ok!(sorg.list_classes().await).len());
}
