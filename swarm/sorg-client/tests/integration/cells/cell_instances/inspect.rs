use claims::{assert_err, assert_ok};

use cell_protocol::Sri;

use super::{CLASS_NAME, INSTANCE_SRI, seed_instance, sorg_client};
use crate::integration::spawn_db_test_app;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn happy_path() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());
    let sri = Sri::from_target(INSTANCE_SRI).unwrap();

    // Arrange — seed an instance in the registry
    seed_instance(test_app.session(), &sri, CLASS_NAME).await;

    // Act
    let inspected = assert_ok!(
        sorg.inspect_instance(&sri).await,
        "inspect_instance should succeed"
    );

    // Assert — returned info matches what was stored
    assert_eq!(sri, inspected.sri, "sri should match");
    assert_eq!(CLASS_NAME, inspected.class_name, "class name should match");
    assert!(
        inspected.lineage.parent.is_none(),
        "a directly-registered instance has no parent"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn not_found() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());
    let sri = Sri::from_target(INSTANCE_SRI).unwrap();

    // Act — inspect an SRI that was never created
    assert_err!(
        sorg.inspect_instance(&sri).await,
        "inspect_instance for missing instance should fail"
    );
}
