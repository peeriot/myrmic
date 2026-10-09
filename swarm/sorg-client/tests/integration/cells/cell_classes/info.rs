use cell_protocol::BlobHash;
use claims::{assert_none, assert_ok};

use super::{CLASS_NAME, DUMMY_BINARY, sorg_client};
use crate::integration::spawn_db_test_app;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn unknown_id_has_no_info() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    assert_none!(assert_ok!(
        sorg.get_class_info(BlobHash::of(DUMMY_BINARY)).await
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn unknown_name_resolves_to_nothing() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    assert_none!(assert_ok!(sorg.resolve_class_name(CLASS_NAME).await));
}
