use std::collections::HashSet;

use cell_protocol::BlobHash;
use claims::assert_ok;

use super::{CLASS_NAME, DUMMY_BINARY, OTHER_BINARY, OTHER_NAME, sorg_client};
use crate::integration::spawn_db_test_app;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn empty_registry() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    assert!(assert_ok!(sorg.list_classes().await).is_empty());
    assert!(assert_ok!(sorg.list_class_names().await).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn lists_classes_and_their_names() {
    let test_app = spawn_db_test_app().await;
    let sorg = sorg_client(test_app.session());

    // Arrange
    let first = assert_ok!(
        sorg.add_class(CLASS_NAME, DUMMY_BINARY.to_vec(), vec![])
            .await
    );
    let second = assert_ok!(
        sorg.add_class(OTHER_NAME, OTHER_BINARY.to_vec(), vec![])
            .await
    );

    // Act
    let classes = assert_ok!(sorg.list_classes().await);
    let names = assert_ok!(sorg.list_class_names().await);

    // Assert
    let ids: HashSet<BlobHash> = classes.into_iter().map(|(id, _)| id).collect();
    assert_eq!(ids, HashSet::from([first, second]));
    let names: HashSet<(String, BlobHash)> = names.into_iter().collect();
    assert_eq!(
        names,
        HashSet::from([
            (CLASS_NAME.to_owned(), first),
            (OTHER_NAME.to_owned(), second),
        ])
    );
}
