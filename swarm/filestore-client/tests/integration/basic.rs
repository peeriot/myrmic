use std::time::Duration;

use claims::{assert_err, assert_none, assert_ok, assert_some};
use sorg_tests::{data_file, enable_test_logging, load_into_db};

use crate::integration::{set_up_client, set_up_file_store, set_up_swarm_without_file_store};

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn fs_presence_reported() {
    // Arrange - client (other session than the fs, since this will be typical)
    enable_test_logging("debug");
    let client = set_up_client().await;

    // Assert I - no fs present
    assert!(!client.is_fs_present().await);

    // Act II - deploy filestore
    let isolated = set_up_file_store().await;
    tokio::time::sleep(Duration::from_millis(1)).await;

    // Assert II - fs present
    assert!(isolated.client.is_fs_present().await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn fs_absence_reported() {
    // Arrange - client (other session than the fs, since this will be typical)
    enable_test_logging("debug");
    let client = set_up_client().await;

    // Assert I - no fs present
    assert!(!client.is_fs_present().await);

    // Act II - deploy swarm without filestore
    let isolated = set_up_swarm_without_file_store().await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Assert II - fs still absent
    assert!(!isolated.client.is_fs_present().await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn file_presence_reported() {
    enable_test_logging("debug");

    // Arrange - client (other session than the fs, since this will be typical)
    let client = set_up_client().await;
    let file_path_present = "/example.txt";
    let file_path_absent = "/absent.txt";

    // Assert I - we get an error when there is no filestore
    assert_err!(client.is_file_present(file_path_present).await);

    // Act II - deploy filestore
    let isolated = set_up_file_store().await;
    load_into_db(data_file!("example.txt"), &isolated.handle).await;

    // Assert II - check that present/absent files are reported as such
    assert!(
        isolated
            .client
            .is_file_present(file_path_present)
            .await
            .unwrap()
    );
    assert!(
        !isolated
            .client
            .is_file_present(file_path_absent)
            .await
            .unwrap()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn file_provided() {
    enable_test_logging("debug");
    // Arrange - client (other session than the fs, since this will be typical)
    let client = set_up_client().await;
    let file_path_present = "/example.txt";
    let file_path_absent = "/absent.txt";

    // Assert I - we get an error when there is no filestore
    assert_err!(client.get_file(file_path_present).await);

    // Act II - deploy filestore
    let isolated = set_up_file_store().await;
    load_into_db(data_file!("example.txt"), &isolated.handle).await;

    // Assert II - check that we get the file which is there and a 'None' for the absent file
    assert_none!(isolated.client.get_file(file_path_absent).await.unwrap());
    let bytes = assert_some!(isolated.client.get_file(file_path_present).await.unwrap());
    let expected_str = "example file";
    let read_str = String::from_utf8(bytes).unwrap();
    assert_eq!(expected_str, read_str);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn file_storage_and_deletion() {
    enable_test_logging("debug");

    // Arrange - client (other session than the fs, since this will be typical)
    let client = set_up_client().await;
    let file_path_store = "/data/blah.txt";
    let content = "42";
    let content_bytes = content.as_bytes().to_vec();

    // Assert I - we get an error when there is no filestore
    assert_err!(
        client
            .store_file(file_path_store, content_bytes.clone())
            .await
    );

    // Act II - deploy filestore and store the file
    let isolated = set_up_file_store().await;
    let client = isolated.client;

    assert!(!client.is_file_present(file_path_store).await.unwrap());
    assert_ok!(
        client
            .store_file(file_path_store, content_bytes.clone())
            .await
    );

    // Assert II - assert that the file is now present and we can read it to get the content we've written
    assert!(client.is_file_present(file_path_store).await.unwrap());
    let content_read = assert_some!(client.get_file(file_path_store).await.unwrap());
    assert_eq!(content_bytes, content_read);

    // Act III - delete the file
    assert_ok!(client.delete_file(file_path_store).await);

    // Assert III - assert that the file is absent again
    assert!(!client.is_file_present(file_path_store).await.unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn store_file_hashed_returns_hash() {
    enable_test_logging("debug");
    let isolated = set_up_file_store().await;
    let client = isolated.client;

    let name = "my-cell";
    let content = vec![1, 2, 3, 4, 5];

    // Act — store a file and get its content hash
    let hash = assert_ok!(client.store_file_hashed(name, content.clone()).await);

    // Assert — hash is non-empty and file is retrievable by path
    assert!(!hash.is_empty());
    let retrieved = assert_some!(assert_ok!(client.get_file(name).await));
    assert_eq!(content, retrieved);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn get_file_by_hash_happy_path() {
    enable_test_logging("debug");
    let isolated = set_up_file_store().await;
    let client = isolated.client;

    let name = "my-cell";
    let content = vec![1, 2, 3, 4, 5];

    // Arrange — store a file and get its content hash
    let hash = assert_ok!(client.store_file_hashed(name, content.clone()).await);

    // Act + Assert — retrieve by hash, verify bytes match
    let retrieved = assert_some!(assert_ok!(client.get_file_by_hash(&hash).await));
    assert_eq!(content, retrieved);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn get_file_by_hash_not_found() {
    enable_test_logging("debug");
    let isolated = set_up_file_store().await;
    let client = isolated.client;

    // Act + Assert — query a bogus hash
    assert_none!(assert_ok!(
        client
            .get_file_by_hash("000000000000000000000000000000000000000000000000000000000000dead")
            .await
    ));
}
