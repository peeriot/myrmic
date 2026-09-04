use claims::assert_ok;

use cell_protocol::Sri;

use super::{CLASS_NAME, INSTANCE_SRI, seed_instance, sorg_client};
use crate::integration::spawn_db_test_app;

const INSTANCE_SRI_B: &str = "test-instance-b";

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn empty() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Act
    let instances = assert_ok!(sorg.list_instances().await, "list_instances should succeed");

    // Assert
    assert!(
        instances.is_empty(),
        "list should be empty when no instances exist"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn returns_created_instances() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());
    let sri_a = Sri::from_target(INSTANCE_SRI).unwrap();
    let sri_b = Sri::from_target(INSTANCE_SRI_B).unwrap();

    // Arrange — seed two instances
    seed_instance(test_app.session(), &sri_a, CLASS_NAME).await;
    seed_instance(test_app.session(), &sri_b, CLASS_NAME).await;

    // Act
    let instances = assert_ok!(sorg.list_instances().await, "list_instances should succeed");

    // Assert — both instances present with correct fields
    assert_eq!(2, instances.len(), "both instances should be listed");
    assert!(
        instances
            .iter()
            .any(|i| i.sri == sri_a && i.class_name == CLASS_NAME),
        "first instance should appear with correct sri and class name"
    );
    assert!(
        instances
            .iter()
            .any(|i| i.sri == sri_b && i.class_name == CLASS_NAME),
        "second instance should appear with correct sri and class name"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn reflects_erasure() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());
    let sri_a = Sri::from_target(INSTANCE_SRI).unwrap();
    let sri_b = Sri::from_target(INSTANCE_SRI_B).unwrap();

    // Arrange — seed two instances, erase one
    seed_instance(test_app.session(), &sri_a, CLASS_NAME).await;
    seed_instance(test_app.session(), &sri_b, CLASS_NAME).await;
    assert_ok!(
        sorg.erase_instance(&sri_a).await,
        "erase_instance should succeed"
    );

    // Act
    let instances = assert_ok!(sorg.list_instances().await, "list_instances should succeed");

    // Assert — only the remaining instance is listed
    assert_eq!(
        1,
        instances.len(),
        "only one instance should remain after erasure"
    );
    assert_eq!(
        sri_b, instances[0].sri,
        "remaining instance should be the one that was not erased"
    );
}
