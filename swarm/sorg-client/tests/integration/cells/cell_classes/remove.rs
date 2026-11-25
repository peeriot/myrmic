use cell_protocol::{ArtifactPlatform, BlobHash, Sri};
use claims::{assert_none, assert_ok, assert_some};
use myrmic_common::cells::spawn_ref::SPAWN_REF_MAGIC;
use sorg_common::class_registry::{ClassReference, Removal};
use sorg_common::{CellConfig, CellDeployment, root_restart};

use super::{
    AOT_BINARY, CLASS_NAME, DUMMY_BINARY, INSTANCE_SRI, OTHER_BINARY, OTHER_NAME, seed_instance,
    sorg_client,
};
use crate::integration::spawn_db_test_app;

const PLATFORM: ArtifactPlatform = ArtifactPlatform::Riscv32imac;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn unreferenced_class_is_removed() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange
    let id = assert_ok!(
        sorg.add_class(CLASS_NAME, DUMMY_BINARY.to_vec(), vec![])
            .await
    );

    // Act
    assert!(assert_ok!(sorg.unbind_class_name(CLASS_NAME, id).await));
    let removal = assert_ok!(sorg.remove_class_if_unreferenced(id).await);

    // Assert
    assert_eq!(removal, Removal::Removed);
    assert_none!(assert_ok!(sorg.get_class_info(id).await));
    assert!(assert_ok!(sorg.list_classes().await).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn unbinding_leaves_a_name_pointing_elsewhere_alone() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange
    let id = assert_ok!(
        sorg.add_class(CLASS_NAME, DUMMY_BINARY.to_vec(), vec![])
            .await
    );

    // Act
    let unbound = assert_ok!(
        sorg.unbind_class_name(CLASS_NAME, BlobHash::of(OTHER_BINARY))
            .await
    );

    // Assert
    assert!(!unbound);
    assert_eq!(
        assert_ok!(sorg.resolve_class_name(CLASS_NAME).await),
        Some(id)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn class_with_another_name_is_kept() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange — the same wasm under two names
    let id = assert_ok!(
        sorg.add_class(CLASS_NAME, DUMMY_BINARY.to_vec(), vec![])
            .await
    );
    assert_ok!(
        sorg.add_class(OTHER_NAME, DUMMY_BINARY.to_vec(), vec![])
            .await
    );

    // Act
    assert!(assert_ok!(sorg.unbind_class_name(CLASS_NAME, id).await));
    let removal = assert_ok!(sorg.remove_class_if_unreferenced(id).await);

    // Assert
    assert_eq!(
        removal,
        Removal::Kept(vec![ClassReference::Name(OTHER_NAME.to_owned())])
    );
    assert_some!(assert_ok!(sorg.get_class_info(id).await));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn class_with_an_instance_is_kept() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange
    let id = assert_ok!(
        sorg.add_class(CLASS_NAME, DUMMY_BINARY.to_vec(), vec![])
            .await
    );
    let sri = Sri::of_path(INSTANCE_SRI).unwrap();
    seed_instance(test_app.session(), &sri, id).await;

    // Act
    assert!(assert_ok!(sorg.unbind_class_name(CLASS_NAME, id).await));
    let removal = assert_ok!(sorg.remove_class_if_unreferenced(id).await);

    // Assert
    assert_eq!(removal, Removal::Kept(vec![ClassReference::Instance(sri)]));
    assert_some!(assert_ok!(sorg.get_class_info(id).await));
}

/// A root that will be restarted needs its class even while no instance of it
/// exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn class_with_a_restart_spec_is_kept() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange
    let id = assert_ok!(
        sorg.add_class(CLASS_NAME, DUMMY_BINARY.to_vec(), vec![])
            .await
    );
    let sri = Sri::of_path(INSTANCE_SRI).unwrap();
    assert_ok!(
        root_restart::write_spec(
            test_app.session(),
            &CellDeployment::new(sri, CellConfig::Wasm { class: id }),
        )
        .await
    );

    // Act
    assert!(assert_ok!(sorg.unbind_class_name(CLASS_NAME, id).await));
    let removal = assert_ok!(sorg.remove_class_if_unreferenced(id).await);

    // Assert
    assert_eq!(
        removal,
        Removal::Kept(vec![ClassReference::RestartSpec(sri)])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn aot_goes_with_its_wasm_class() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange
    let id = assert_ok!(
        sorg.add_class(
            CLASS_NAME,
            DUMMY_BINARY.to_vec(),
            vec![(PLATFORM, AOT_BINARY.to_vec())]
        )
        .await
    );

    // Act
    assert!(assert_ok!(sorg.unbind_class_name(CLASS_NAME, id).await));
    let removal = assert_ok!(sorg.remove_class_if_unreferenced(id).await);

    // Assert
    assert_eq!(removal, Removal::Removed);
    assert_none!(assert_ok!(
        sorg.get_class_info(BlobHash::of(AOT_BINARY)).await
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn aot_shared_with_another_wasm_class_stays() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange — two wasm classes compiled to the same AOT binary
    let id = assert_ok!(
        sorg.add_class(
            CLASS_NAME,
            DUMMY_BINARY.to_vec(),
            vec![(PLATFORM, AOT_BINARY.to_vec())]
        )
        .await
    );
    assert_ok!(
        sorg.add_class(
            OTHER_NAME,
            OTHER_BINARY.to_vec(),
            vec![(PLATFORM, AOT_BINARY.to_vec())]
        )
        .await
    );

    // Act
    assert!(assert_ok!(sorg.unbind_class_name(CLASS_NAME, id).await));
    let removal = assert_ok!(sorg.remove_class_if_unreferenced(id).await);

    // Assert — the class is gone, the AOT the other class needs is not
    assert_eq!(removal, Removal::Removed);
    assert_some!(assert_ok!(
        sorg.get_class_info(BlobHash::of(AOT_BINARY)).await
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn aot_of_a_wasm_class_is_kept() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange
    let id = assert_ok!(
        sorg.add_class(
            CLASS_NAME,
            DUMMY_BINARY.to_vec(),
            vec![(PLATFORM, AOT_BINARY.to_vec())]
        )
        .await
    );

    // Act
    let removal = assert_ok!(
        sorg.remove_class_if_unreferenced(BlobHash::of(AOT_BINARY))
            .await
    );

    // Assert
    assert_eq!(removal, Removal::Kept(vec![ClassReference::Class(id)]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn class_spawned_by_another_class_is_kept() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange — a child class without a name, and a parent whose wasm spawns it
    let child = assert_ok!(
        sorg.add_class(OTHER_NAME, OTHER_BINARY.to_vec(), vec![])
            .await
    );
    assert!(assert_ok!(sorg.unbind_class_name(OTHER_NAME, child).await));
    let parent = assert_ok!(sorg.add_class(CLASS_NAME, spawning(child), vec![]).await);

    // Act
    let removal = assert_ok!(sorg.remove_class_if_unreferenced(child).await);

    // Assert
    assert_eq!(removal, Removal::Kept(vec![ClassReference::Class(parent)]));
}

/// A wasm stand-in with one spawn reference, patched to `child` as a deploy patches it.
fn spawning(child: BlobHash) -> Vec<u8> {
    let BlobHash::Sha2(hash) = child;
    let name = "child";
    let mut wasm = SPAWN_REF_MAGIC.to_vec();
    wasm.extend_from_slice(&hash);
    wasm.extend_from_slice(
        &u32::try_from(name.len())
            .expect("a short name")
            .to_le_bytes(),
    );
    wasm.extend_from_slice(name.as_bytes());
    wasm
}
