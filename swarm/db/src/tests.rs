use crate::domain;
use crate::domain::{FieldValue, Key, Scope, Timestamp};
use crate::store::fjall::{Store, Transaction};
use crate::store::{TransactionMode, TransactionOptions};
use crate::utils::display_bytes;
use db_commons::models::{TbOrderBy, TsOrderBy};

use skey::StoreKey;
use std::time::Duration;

mod replication;
mod semantic;
mod snapshots;

fn open_tmp() -> Store {
    let opts = crate::store::Options::test();
    // Just in case, we push the clock past 0, which surrealkv uses as a sentinal.
    let _ = opts.logic_clock.new_timestamp();

    Store::init(opts).expect("Unable to open storage")
}

fn write(store: &Store) -> Transaction {
    store
        .begin_local(&TransactionOptions::write())
        .expect("unable to start tx")
}

fn read(store: &Store) -> Transaction {
    store
        .begin_local(&TransactionOptions::read())
        .expect("unable to start tx")
}

#[tokio::test(flavor = "current_thread")]
async fn basic_key_value() {
    const KEY: &str = "my-fancy-key";

    const VALUE: &[u8] = br#"
    {
        "thing": "lol"
    }
    "#;

    let store = open_tmp();

    let scope = Scope::default();

    let mut tx = write(&store);

    let value = tx.key_get(scope.kv(KEY)).expect("unable to read db");
    assert!(value.is_none(), "Nothing should be stored");

    tx.key_put(scope.kv(KEY), VALUE).expect("unable to read db");

    {
        // Double check transaction isolation.
        let mut tx = write(&store);

        let value = tx.key_get(scope.kv(KEY)).expect("unable to read db");
        assert!(value.is_none(), "Nothing should be stored");
    }

    let value = tx.key_get(scope.kv(KEY)).expect("unable to read db");
    assert!(value.is_some(), "Something should be stored");

    tx.commit().expect("unable to commit");

    {
        let mut tx = read(&store);

        let value = tx.key_get(scope.kv(KEY)).expect("unable to read db");
        assert!(value.is_some(), "Something should be stored");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn test_key_prefix() {
    const VALUE: &[u8] = b"{\"key\": \"value\"}";

    const KEYS: &[&str] = &["region-a-1", "region-a-2", "region-b-3", "region-b-4"];

    let store = open_tmp();

    let scope = Scope::default();

    {
        let mut tx = write(&store);

        for key in KEYS {
            tx.key_put(scope.kv(key), VALUE).expect("unable to insert");
        }

        tx.commit().expect("unable to commit");
    }

    {
        let mut tx = read(&store);

        let mut keys = tx.key_prefix(scope, "region-").expect("unable to query db");
        keys.sort_unstable();

        assert_eq!(keys.len(), 4);
        for (a, b) in keys.iter().zip(KEYS) {
            assert_eq!(a, b);
        }
    }

    {
        let mut tx = read(&store);

        let mut keys = tx
            .key_prefix(scope, "region-a-")
            .expect("unable to query db");
        keys.sort_unstable();

        assert_eq!(keys.len(), 2);
        for (a, b) in keys.iter().zip(KEYS.iter().take(2)) {
            assert_eq!(a, b);
        }
    }

    {
        let mut tx = read(&store);

        let mut keys = tx
            .key_prefix(scope, "region-b-")
            .expect("unable to query db");
        keys.sort_unstable();

        assert_eq!(keys.len(), 2);
        for (a, b) in keys.iter().zip(KEYS.iter().skip(2)) {
            assert_eq!(a, b);
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn repro_concurrent_same_scope_disjoint_keys() {
    // Two transactions touch the SAME scope but DISJOINT keys (like two cells
    // appending events to the shared `@events/<name>` scope). Commit A then B.
    let store = open_tmp();
    let scope = Scope::default();

    let mut tx_b = write(&store);
    tx_b.key_put(scope.kv("b"), b"vb").expect("b put");

    let mut tx_a = write(&store);
    tx_a.key_put(scope.kv("a"), b"va").expect("a put");
    tx_a.commit().expect("a commit");

    let res = tx_b.commit();
    assert!(res.is_ok(), "B should commit despite A: {:?}", res.err());
}

#[tokio::test(flavor = "current_thread")]
async fn repro_concurrent_distinct_scopes() {
    let store = open_tmp();
    let scope_a = Key::new_scope("ns", "a", "d");
    let scope_b = Key::new_scope("ns", "b", "d");

    let mut tx_b = write(&store);
    tx_b.key_put(scope_b.kv("k"), b"vb").expect("b put");

    let mut tx_a = write(&store);
    tx_a.key_put(scope_a.kv("k"), b"va").expect("a put");
    tx_a.commit().expect("a commit");

    let res = tx_b.commit();
    assert!(
        res.is_ok(),
        "distinct scopes must not conflict: {:?}",
        res.err()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn conflict_detected_on_overlapping_read_write() {
    // The read-tracking opt-out in `commit` is scoped to the sync-point parent
    // lookup; genuine read/write conflicts must still abort. tx_b reads `k`,
    // then tx_a overwrites `k` and commits first, so tx_b's commit must fail.
    let store = open_tmp();
    let scope = Scope::default();

    {
        let mut tx = write(&store);
        tx.key_put(scope.kv("k"), b"v0").expect("seed put");
        tx.commit().expect("seed commit");
    }

    let mut tx_b = write(&store);
    let _ = tx_b.key_get(scope.kv("k")).expect("b read");

    let mut tx_a = write(&store);
    tx_a.key_put(scope.kv("k"), b"v1").expect("a put");
    tx_a.commit().expect("a commit");

    tx_b.key_put(scope.kv("b"), b"vb").expect("b put");
    let res = tx_b.commit();
    assert!(res.is_err(), "B read `k` that A overwrote — must conflict");
}

#[tokio::test(flavor = "current_thread")]
async fn test_retention() {
    const KEY: &str = "my-fancy-key";

    const VALUE: &[u8] = br#"
    {
        "thing": "lol"
    }
    "#;

    let store = open_tmp();
    let clock = store.clock();
    let scope = Scope::default();

    {
        let mut tx = write(&store);
        tx.key_put(scope.kv(KEY), VALUE).expect("unable to read db");
        tx.commit().expect("unable to commit");
    }

    {
        let mut tx = store
            .begin_local(&TransactionOptions::retain_for(
                TransactionMode::ReadWrite,
                Duration::from_nanos(5),
            ))
            .expect("unable to start tx");
        tx.key_put(scope.kv(KEY), b"this is a special value")
            .expect("unable to read db");
        tx.commit().expect("unable to commit");
    }

    {
        let mut tx = write(&store);

        let value = tx
            .key_get(scope.kv(KEY))
            .expect("unable to read db")
            .expect("should be there");
        assert_eq!(
            display_bytes(&value),
            display_bytes(b"this is a special value")
        );

        tx.rollback();
    }

    // push the clock forward.
    for _ in 0..50 {
        clock.new_timestamp();
    }

    // force a gc
    store.perform_gc().unwrap();

    {
        let mut tx = write(&store);

        let value = tx
            .key_get(scope.kv(KEY))
            .expect("unable to read db")
            .expect("should be there");
        assert_eq!(display_bytes(&value), display_bytes(VALUE));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn basic_table() {
    const TABLE: &str = "entity";

    const EID: &[u8] = b"1337";

    const VALUE: &[u8] = br#"
    {
        "hello": "world"
    }
    "#;

    let store = open_tmp();

    let scope = Scope::default();

    let mut tx = write(&store);

    let value = tx
        .tb_get(scope.table(TABLE), EID)
        .expect("unable to read db");
    assert!(value.is_none(), "Nothing should be stored");

    tx.tb_insert(scope.table(TABLE), EID, VALUE)
        .expect("unable to read db");

    let count = tx
        .tb_count(scope.table(TABLE))
        .expect("unable to count table");
    assert_eq!(count, 1);

    let value = tx
        .tb_get(scope.table(TABLE), EID)
        .expect("unable to read db");
    assert!(value.is_some(), "Something should be stored");

    {
        // Double check transaction isolation.
        let mut tx = write(&store);

        let value = tx
            .tb_get(scope.table(TABLE), EID)
            .expect("unable to read db");
        assert!(value.is_none(), "Nothing should be stored");
    }

    tx.commit().expect("unable to commit");

    let mut tx = read(&store);

    let value = tx
        .tb_get(scope.table(TABLE), EID)
        .expect("unable to read db");
    assert!(value.is_some(), "Something should be stored");
}

#[tokio::test(flavor = "current_thread")]
async fn basic_table_list() {
    const TABLE: &str = "entity";

    let store = open_tmp();

    let scope = Scope::default();

    {
        let mut tx = write(&store);

        for i in 0..50 {
            let eid = uuid::Uuid::now_v7();
            let eid = eid.as_bytes().as_slice();

            tx.tb_insert(scope.table(TABLE), eid, format!("{}", i).as_bytes())
                .expect("unable to read db");
        }
        tx.commit().expect("unable to commit");
    }

    let mut tx = read(&store);

    {
        let value = tx
            .tb_list(scope.table(TABLE), None, Some(10), None)
            .expect("unable to read db");
        assert_eq!(value.len(), 10);
    }

    {
        let value = tx
            .tb_list(scope.table(TABLE), None, Some(50), None)
            .expect("unable to read db");
        assert_eq!(value.len(), 50);
    }
    {
        let value = tx
            .tb_list(scope.table(TABLE), None, None, None)
            .expect("unable to read db");
        assert_eq!(value.len(), 50);
    }

    {
        let page_size = 10;

        let value = tx
            .tb_list(scope.table(TABLE), None, Some(page_size), None)
            .expect("unable to read db");
        assert_eq!(value.len(), page_size);

        for (index, (id, v)) in value.iter().enumerate() {
            let _id = uuid::Uuid::from_slice(id)
                .expect("this should be a uuid, because we use a v7 by default");
            assert_eq!(v, &format!("{}", index).as_bytes());
        }

        let id = value
            .last()
            .map(|(id, _)| id.clone())
            .expect("we checked the length already");

        {
            let mut value = tx
                .tb_list(
                    scope.table(TABLE),
                    Some(domain::Cursor::At(id.clone())),
                    Some(1),
                    None,
                )
                .expect("unable to read db");
            assert_eq!(value.len(), 1);
            let (_id, value) = value.pop().expect("we just checked the size");
            assert_eq!(&value, b"9");
        }

        let value = tx
            .tb_list(
                scope.table(TABLE),
                Some(domain::Cursor::After(id.clone())),
                Some(page_size),
                None,
            )
            .expect("unable to read db");
        assert_eq!(value.len(), page_size);

        for (index, (id, v)) in value.iter().enumerate() {
            let _id = uuid::Uuid::from_slice(id)
                .expect("this should be a uuid, because we use a v7 by default");
            assert_eq!(v, &format!("{}", page_size + index).as_bytes());
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn test_table_order() {
    const TABLE: &str = "entity";

    fn ids(
        store: &Store,
        cursor: Option<domain::Cursor>,
        limit: Option<usize>,
        order: Option<TbOrderBy>,
    ) -> Vec<Vec<u8>> {
        let scope = Scope::default();
        let mut tx = read(store);
        tx.tb_list(scope.table(TABLE), cursor, limit, order)
            .expect("unable to read db")
            .into_iter()
            .map(|(id, _value)| id)
            .collect::<Vec<_>>()
    }

    let store = open_tmp();
    let scope = Scope::default();

    {
        let mut tx = write(&store);
        for k in [b"a", b"b", b"c", b"d"] {
            tx.tb_insert(scope.table(TABLE), k.as_slice(), k.as_slice())
                .expect("unable to insert");
        }
        tx.commit().expect("unable to commit");
    }

    let key = |b: &[u8]| b.to_vec();

    // Unspecified order defaults to ascending by key.
    assert_eq!(
        ids(&store, None, None, None),
        vec![key(b"a"), key(b"b"), key(b"c"), key(b"d")]
    );
    assert_eq!(
        ids(&store, None, None, Some(TbOrderBy::KeyAsc)),
        vec![key(b"a"), key(b"b"), key(b"c"), key(b"d")]
    );
    assert_eq!(
        ids(&store, None, None, Some(TbOrderBy::KeyDesc)),
        vec![key(b"d"), key(b"c"), key(b"b"), key(b"a")]
    );

    // limit + descending returns the greatest N, not the smallest N reversed.
    assert_eq!(
        ids(&store, None, Some(2), Some(TbOrderBy::KeyDesc)),
        vec![key(b"d"), key(b"c")]
    );
    assert_eq!(
        ids(&store, None, Some(2), Some(TbOrderBy::KeyAsc)),
        vec![key(b"a"), key(b"b")]
    );

    // The cursor is relative to iteration direction. Descending `After(c)` yields
    // the keys strictly below `c`, newest-first.
    assert_eq!(
        ids(
            &store,
            Some(domain::Cursor::After(key(b"c"))),
            None,
            Some(TbOrderBy::KeyDesc)
        ),
        vec![key(b"b"), key(b"a")]
    );
    // Descending `At(c)` includes `c` and everything below it.
    assert_eq!(
        ids(
            &store,
            Some(domain::Cursor::At(key(b"c"))),
            None,
            Some(TbOrderBy::KeyDesc)
        ),
        vec![key(b"c"), key(b"b"), key(b"a")]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn basic_table_insert_batched() {
    const TABLE: &str = "entity";

    let store = open_tmp();

    let scope = Scope::default();

    let entries: Vec<(&[u8], &[u8])> = vec![
        (b"a".as_slice(), b"value-a".as_slice()),
        (b"b".as_slice(), b"value-b".as_slice()),
        (b"c".as_slice(), b"value-c".as_slice()),
    ];

    {
        let mut tx = write(&store);

        tx.tb_insert_batched(scope.table(TABLE), &entries)
            .expect("unable to write batch");

        // Visible within the same tx.
        assert_eq!(
            tx.tb_count(scope.table(TABLE)).expect("unable to count"),
            entries.len()
        );

        tx.commit().expect("unable to commit");
    }

    let mut tx = read(&store);

    assert_eq!(
        tx.tb_count(scope.table(TABLE)).expect("unable to count"),
        entries.len()
    );

    for (eid, value) in &entries {
        let stored = tx
            .tb_get(scope.table(TABLE), eid)
            .expect("unable to read db");
        assert_eq!(stored.as_deref(), Some(*value));
    }
}

/// Delete then re-insert the same key in one tx: the re-insert must win.
/// (Delete sorts ahead of insert at the shared ts, so the delete masked it.)
#[tokio::test(flavor = "current_thread")]
async fn delete_then_reinsert_same_entry_in_tx() {
    const TABLE: &str = "entity";
    const EID: &[u8] = b"class-b";
    const OLD: &[u8] = b"old value";
    const NEW: &[u8] = b"new value";

    let store = open_tmp();
    let scope = Scope::default();

    // Arrange — a committed entry.
    {
        let mut tx = write(&store);
        tx.tb_insert(scope.table(TABLE), EID, OLD)
            .expect("unable to insert");
        tx.commit().expect("unable to commit");
    }

    // Act — delete then re-insert the same eid within one transaction.
    {
        let mut tx = write(&store);
        tx.tb_delete(scope.table(TABLE), EID)
            .expect("unable to delete");
        tx.tb_insert(scope.table(TABLE), EID, NEW)
            .expect("unable to re-insert");

        // The re-insert is visible within the same transaction.
        let value = tx
            .tb_get(scope.table(TABLE), EID)
            .expect("unable to read db")
            .expect("entry should still be present after delete-then-insert");
        assert_eq!(display_bytes(&value), display_bytes(NEW));

        // And it shows up in a listing (this is what `list_classes` returned 0 for).
        let listed = tx
            .tb_list(scope.table(TABLE), None, None, None)
            .expect("unable to list");
        assert_eq!(listed.len(), 1);
        assert_eq!(display_bytes(&listed[0].1), display_bytes(NEW));

        tx.commit().expect("unable to commit");
    }

    // Assert — still present with the new value after commit.
    {
        let mut tx = read(&store);
        let value = tx
            .tb_get(scope.table(TABLE), EID)
            .expect("unable to read db")
            .expect("entry should survive commit");
        assert_eq!(display_bytes(&value), display_bytes(NEW));

        let listed = tx
            .tb_list(scope.table(TABLE), None, None, None)
            .expect("unable to list");
        assert_eq!(listed.len(), 1);
    }
}

/// Inverse: insert then delete in one tx still leaves it deleted.
#[tokio::test(flavor = "current_thread")]
async fn insert_then_delete_same_entry_in_tx() {
    const TABLE: &str = "entity";
    const EID: &[u8] = b"ephemeral";

    let store = open_tmp();
    let scope = Scope::default();

    {
        let mut tx = write(&store);
        tx.tb_insert(scope.table(TABLE), EID, b"value")
            .expect("unable to insert");
        tx.tb_delete(scope.table(TABLE), EID)
            .expect("unable to delete");

        let value = tx
            .tb_get(scope.table(TABLE), EID)
            .expect("unable to read db");
        assert!(
            value.is_none(),
            "delete is the last write, entry must be gone"
        );

        tx.commit().expect("unable to commit");
    }

    {
        let mut tx = read(&store);
        let value = tx
            .tb_get(scope.table(TABLE), EID)
            .expect("unable to read db");
        assert!(value.is_none(), "entry must stay deleted after commit");
    }
}

/// This is kind of a hodge podge of different things, but mostly blob related.
/// This is testing initial storing, linking, and listing.
/// Making sure effects aren't visible if they're not committed yet.
#[tokio::test(flavor = "current_thread")]
async fn basic_blob_test() {
    const BLOB: &[u8] = br"
    This is my fancy blob that contains fancy stuff.

    Anything is possible!
    ";

    let store = open_tmp();

    let scope = Scope::default();

    let mut tx = write(&store);

    let id = tx
        .store_blob(scope, BLOB)
        .expect("should be able to store blob");

    {
        let second_id = tx
            .store_blob(scope, BLOB)
            .expect("should be able to store blob");

        assert_eq!(
            id.encode().unwrap(),
            second_id.encode().unwrap(),
            "Same data should have same ids"
        );

        let mut blob = BLOB.to_vec();
        blob.extend_from_slice(b"\nEven more fancy stuff!");

        let second_id = tx
            .store_blob(scope, &blob)
            .expect("should be able to store blob");

        assert_ne!(
            id.encode().unwrap(),
            second_id.encode().unwrap(),
            "Different data should have different ids"
        );
    }

    let resolved = tx
        .resolve_blob(id)
        .expect("unable to read db")
        .expect("Unable to locate blob");

    assert_eq!(resolved.as_slice(), BLOB, "Blob contents differ");

    let paths = tx.list_paths(scope, None).expect("unable to read db");
    assert_eq!(paths, Vec::<String>::new(), "Found existing files.");

    tx.link_blob(scope.path("/test2.txt"), id)
        .expect("unable to read db");

    let (resolved, _) = tx
        .resolve_path(scope.path("/test2.txt"))
        .expect("unable to read db")
        .expect("Unable to locate blob");

    assert_eq!(resolved.as_slice(), BLOB, "Blob contents differ");

    let paths = tx.list_paths(scope, None).expect("unable to read db");
    assert_eq!(
        paths,
        vec![String::from("/test2.txt")],
        "Found existing files."
    );

    tx.link_blob(scope.path("/test1.txt"), id)
        .expect("unable to read db");

    let paths = tx.list_paths(scope, None).expect("unable to read db");
    assert_eq!(
        paths,
        vec![String::from("/test1.txt"), String::from("/test2.txt"),],
        "Found existing files."
    );

    // Just double check we're not able to see anything
    {
        let mut tx = write(&store);

        let paths = tx.list_paths(scope, None).expect("unable to read db");

        assert_eq!(paths, Vec::<String>::new(), "No paths should be visible");
    }

    tx.commit().expect("unable to commit to store");

    // Just double check we're not able to see anything
    {
        let mut tx = write(&store);

        let paths = tx.list_paths(scope, None).expect("unable to read db");

        assert_eq!(
            paths,
            vec![String::from("/test1.txt"), String::from("/test2.txt"),],
            "Files should be listed."
        );
    }
}

#[tokio::test(flavor = "current_thread")]
#[allow(clippy::too_many_lines)]
async fn test_time_series() {
    fn find_times(store: &Store, scope: Scope<'_>, measurement: &str) -> Vec<Timestamp> {
        let mut tx = read(store);
        tx.find_measurement(
            scope,
            measurement,
            None,
            None,
            None,
            Some(TsOrderBy::TimestampAsc),
        )
        .expect("unable to read db")
        .into_iter()
        .map(|(_tags, _fields, ts)| ts)
        .collect::<Vec<_>>()
    }

    let store = open_tmp();
    let scope = Scope::default();

    let mut tx = write(&store);

    tx.publish_measurement(
        scope,
        "cpu",
        vec![],
        vec![(String::from("value"), FieldValue::F64(36.54))],
        1,
    )
    .expect("unable to read db");
    tx.publish_measurement(
        scope,
        "cpu",
        vec![],
        vec![(String::from("value"), FieldValue::F64(37.98))],
        2,
    )
    .expect("unable to read db");

    let timestamps = tx
        .find_measurement(
            scope,
            "cpu",
            None,
            None,
            None,
            Some(TsOrderBy::TimestampAsc),
        )
        .expect("unable to read db")
        .into_iter()
        .map(|(_tags, _fields, ts)| ts)
        .collect::<Vec<_>>();
    assert_eq!(timestamps, vec![1, 2]);

    {
        let timestamps = find_times(&store, scope, "cpu");
        assert!(timestamps.is_empty());
    }

    tx.commit().expect("unable to commit");

    {
        let timestamps = find_times(&store, scope, "cpu");
        assert_eq!(timestamps, vec![1, 2]);
    }

    let mut tx = write(&store);
    tx.publish_measurement(
        scope,
        "cpu",
        vec![],
        vec![(String::from("value"), FieldValue::F64(35.02))],
        3,
    )
    .expect("unable to read db");
    tx.publish_measurement(
        scope,
        "cpu",
        vec![],
        vec![(String::from("value"), FieldValue::F64(32.33))],
        4,
    )
    .expect("unable to read db");

    let measurements = tx
        .find_measurement(
            scope,
            "cpu",
            None,
            None,
            None,
            Some(TsOrderBy::TimestampAsc),
        )
        .expect("unable to read db");
    assert_eq!(measurements.len(), 4);

    let timestamps = tx
        .find_measurement(
            scope,
            "cpu",
            None,
            None,
            None,
            Some(TsOrderBy::TimestampAsc),
        )
        .expect("unable to read db")
        .into_iter()
        .map(|(_tags, _fields, ts)| ts)
        .collect::<Vec<_>>();
    assert_eq!(timestamps, vec![1, 2, 3, 4]);
    {
        let timestamps = find_times(&store, scope, "cpu");
        assert_eq!(timestamps, vec![1, 2]);
    }

    let timestamps = tx
        .find_measurement(
            scope,
            "cpu",
            None,
            Some(3),
            None,
            Some(TsOrderBy::TimestampAsc),
        )
        .expect("unable to read db")
        .into_iter()
        .map(|(_tags, _fields, ts)| ts)
        .collect::<Vec<_>>();
    assert_eq!(timestamps, vec![3, 4]);

    let timestamps = tx
        .find_measurement(
            scope,
            "cpu",
            None,
            Some(2),
            Some(4),
            Some(TsOrderBy::TimestampAsc),
        )
        .expect("unable to read db")
        .into_iter()
        .map(|(_tags, _fields, ts)| ts)
        .collect::<Vec<_>>();
    assert_eq!(timestamps, vec![2, 3]);

    let timestamps = tx
        .find_measurement(
            scope,
            "cpu",
            None,
            None,
            Some(3),
            Some(TsOrderBy::TimestampAsc),
        )
        .expect("unable to read db")
        .into_iter()
        .map(|(_tags, _fields, ts)| ts)
        .collect::<Vec<_>>();
    assert_eq!(timestamps, vec![1, 2]);

    let timestamps = tx
        .find_measurement(
            scope,
            "cpu",
            Some(2),
            None,
            None,
            Some(TsOrderBy::TimestampAsc),
        )
        .expect("unable to read db")
        .into_iter()
        .map(|(_tags, _fields, ts)| ts)
        .collect::<Vec<_>>();
    assert_eq!(timestamps, vec![1, 2], "limit caps the result count");

    let timestamps = tx
        .find_measurement(
            scope,
            "cpu",
            Some(1),
            Some(3),
            None,
            Some(TsOrderBy::TimestampAsc),
        )
        .expect("unable to read db")
        .into_iter()
        .map(|(_tags, _fields, ts)| ts)
        .collect::<Vec<_>>();
    assert_eq!(timestamps, vec![3], "limit applies within the start window");

    tx.commit().expect("unable to commit");

    {
        let timestamps = find_times(&store, scope, "cpu");
        assert_eq!(timestamps, vec![1, 2, 3, 4]);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn test_time_series_order() {
    fn times(
        store: &Store,
        scope: Scope<'_>,
        limit: Option<usize>,
        order: Option<TsOrderBy>,
    ) -> Vec<Timestamp> {
        let mut tx = read(store);
        tx.find_measurement(scope, "cpu", limit, None, None, order)
            .expect("unable to read db")
            .into_iter()
            .map(|(_tags, _fields, ts)| ts)
            .collect::<Vec<_>>()
    }

    let store = open_tmp();
    let scope = Scope::default();

    let mut tx = write(&store);
    for ts in [1u64, 2, 3, 4] {
        tx.publish_measurement(
            scope,
            "cpu",
            vec![],
            vec![(String::from("value"), FieldValue::U64(ts))],
            ts,
        )
        .expect("unable to publish");
    }
    tx.commit().expect("unable to commit");

    // Unspecified order defaults to newest-first.
    assert_eq!(times(&store, scope, None, None), vec![4, 3, 2, 1]);
    assert_eq!(
        times(&store, scope, None, Some(TsOrderBy::TimestampDesc)),
        vec![4, 3, 2, 1]
    );
    assert_eq!(
        times(&store, scope, None, Some(TsOrderBy::TimestampAsc)),
        vec![1, 2, 3, 4]
    );

    // limit + descending must return the newest N, not the oldest N reversed.
    assert_eq!(
        times(&store, scope, Some(2), Some(TsOrderBy::TimestampDesc)),
        vec![4, 3]
    );
    assert_eq!(
        times(&store, scope, Some(2), Some(TsOrderBy::TimestampAsc)),
        vec![1, 2]
    );

    // Re-publishing a row must still collapse to one entry, surfacing the
    // newest version, when iterating in descending order.
    let mut tx = write(&store);
    tx.publish_measurement(
        scope,
        "cpu",
        vec![],
        vec![(String::from("value"), FieldValue::U64(99))],
        3,
    )
    .expect("unable to publish");
    tx.commit().expect("unable to commit");

    let mut tx = read(&store);
    let rows = tx
        .find_measurement(
            scope,
            "cpu",
            None,
            None,
            None,
            Some(TsOrderBy::TimestampDesc),
        )
        .expect("unable to read db");
    assert_eq!(
        rows.iter().map(|(_, _, ts)| *ts).collect::<Vec<_>>(),
        vec![4, 3, 2, 1]
    );
    let (_, fields, _) = rows
        .iter()
        .find(|(_, _, ts)| *ts == 3)
        .expect("row at ts 3");
    assert_eq!(fields, &vec![(String::from("value"), FieldValue::U64(99))]);
}

/// This is testing the linking/unlinking behaviour, making sure that everything is correctly updated.
#[tokio::test(flavor = "current_thread")]
async fn blob_link_unlink() {
    const BLOB: &[u8] = br"
    Simple blob
    ";

    const PATH: &str = "/test.txt";

    let store = open_tmp();

    let scope = Scope::default();

    let mut tx = write(&store);

    let id = tx
        .store_blob(scope, BLOB)
        .expect("should be able to store blob");

    {
        let mut tx = write(&store);

        let paths = tx.list_paths(scope, None).expect("unable to read db");

        assert_eq!(paths, Vec::<String>::new(), "No paths should be visible");
    }

    tx.link_blob(scope.path(PATH), id)
        .expect("unable to read db");

    assert_eq!(
        tx.list_paths(scope, None).expect("unable to read db"),
        vec![String::from(PATH)],
        "Files should be found."
    );

    {
        let mut tx = write(&store);

        let paths = tx.list_paths(scope, None).expect("unable to read db");

        assert_eq!(paths, Vec::<String>::new(), "No paths should be visible");
    }

    tx.commit().expect("unable to commit");

    let mut tx = write(&store);

    assert_eq!(
        tx.list_paths(scope, None).expect("unable to read db"),
        vec![String::from(PATH)],
        "Files should be found."
    );

    tx.unlink_blob(scope.path(PATH)).expect("unable to read db");

    let paths = tx.list_paths(scope, None).expect("unable to read db");

    assert_eq!(paths, Vec::<String>::new(), "No paths should be visible");

    tx.commit().expect("unable to commit");

    {
        let mut tx = write(&store);

        let paths = tx.list_paths(scope, None).expect("unable to read db");

        assert_eq!(paths, Vec::<String>::new(), "No paths should be visible");
    }
}

/// This is just checking if we can move a blob from one path to another.
#[tokio::test(flavor = "current_thread")]
async fn blob_move() {
    const BLOB: &[u8] = br"
    Simple blob
    ";

    const OLD_PATH: &str = "/test1.txt";
    const NEW_PATH: &str = "/test2.txt";

    let store = open_tmp();

    let scope = Scope::default();

    {
        let mut tx = write(&store);

        let id = tx
            .store_blob(scope, BLOB)
            .expect("should be able to store blob");

        tx.link_blob(scope.path(OLD_PATH), id)
            .expect("unable to read db");

        tx.commit().expect("unable to commit");
    }

    let mut tx = write(&store);

    assert_eq!(
        tx.list_paths(scope, None).expect("unable to read db"),
        vec![String::from(OLD_PATH)],
        "Files should be found."
    );

    tx.move_blob(scope.path(OLD_PATH), scope.path(NEW_PATH))
        .expect("unable to read db");

    assert_eq!(
        tx.list_paths(scope, None).expect("unable to read db"),
        vec![String::from(NEW_PATH)],
        "Files should be found."
    );

    tx.commit().expect("unable to commit");

    let mut tx = read(&store);

    assert_eq!(
        tx.list_paths(scope, None).expect("unable to read db"),
        vec![String::from(NEW_PATH)],
        "Files should be found."
    );
}

#[tokio::test(flavor = "current_thread")]
async fn basic_changesets() {
    const TABLE: &str = "entity";

    const VALUE: &[u8] = br#"
    {
        "hello": "world"
    }
    "#;

    let store = open_tmp();

    let scope = Scope::default();

    for _ in 0..2 {
        let mut tx = write(&store);

        let eid = uuid::Uuid::now_v7();
        let eid = eid.as_bytes().as_slice();

        tx.tb_insert(scope.table(TABLE), eid, VALUE)
            .expect("unable to read db");

        tx.commit().expect("unable to commit");
    }

    let tx = read(&store);

    let subject = db_commons::models::Subject::Scope(db_commons::models::Scope::new(
        scope.namespace,
        scope.database,
        scope.schema,
    ));

    let mut points = vec![];
    let (lower, upper) =
        domain::SyncPoint::range_from_subject(&subject).expect("unable to construct sp range");

    tx.find_sync_points(lower, upper, |sp, _sm| {
        points.push((sp.epoch, sp.ts, sp.id));
        Ok(())
    })
    .expect("unable to read db");
    assert_eq!(points.len(), 2);
    for &(epoch, ts, id) in &points {
        let sp = Key::sync_point()
            .namespace(scope.namespace)
            .database(scope.database)
            .schema(scope.schema)
            .ts(ts)
            .epoch(epoch)
            .id(id);
        eprintln!("{}/{}/{} @ {}", sp.namespace, sp.database, sp.schema, sp.ts);

        let cs = tx.changeset_for(sp).expect("unable to read db");

        assert_eq!(cs.len(), 1);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn remote_tx_metadata_rides_with_the_tx() {
    let store: Store<Vec<String>> = Store::init(crate::store::Options::test()).unwrap();

    let id = store
        .begin_remote(&TransactionOptions::write())
        .expect("unable to start tx");

    {
        let mut tx = store.find_remote_tx(id).expect("tx must exist");
        assert!(tx.metadata().is_none());
        tx.metadata_or_default().push(String::from("a"));
        tx.metadata_or_default().push(String::from("b"));
    }

    let mut tx = store.remove_remote_tx(id).expect("tx must exist");
    let meta = tx.take_metadata().expect("metadata was set");
    assert_eq!(meta, vec![String::from("a"), String::from("b")]);
    assert!(tx.take_metadata().is_none());

    tx.rollback();
}

#[tokio::test(flavor = "current_thread")]
async fn idle_remote_tx_is_reaped() {
    let mut opts = crate::store::Options::test();
    opts.gc_interval = Some(Duration::from_millis(50));
    let store: Store = Store::init(opts).unwrap();

    let mut tx_opts = TransactionOptions::write();
    tx_opts.idle_timeout = Some(Duration::from_millis(150));

    let reaped = store.begin_remote(&tx_opts).expect("unable to start tx");
    let immortal = store
        .begin_remote(&TransactionOptions::write())
        .expect("unable to start tx");

    tokio::time::sleep(Duration::from_millis(500)).await;

    assert!(
        store.find_remote_tx(reaped).is_none(),
        "idle tx must be reaped"
    );
    assert!(
        store.find_remote_tx(immortal).is_some(),
        "tx without idle timeout must survive"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn touched_tx_survives_the_reaper() {
    let mut opts = crate::store::Options::test();
    opts.gc_interval = Some(Duration::from_millis(50));
    let store: Store = Store::init(opts).unwrap();

    let mut tx_opts = TransactionOptions::write();
    tx_opts.idle_timeout = Some(Duration::from_millis(300));

    let id = store.begin_remote(&tx_opts).expect("unable to start tx");

    // Keep touching well within the timeout, for longer than the timeout.
    for _ in 0..8 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            store.find_remote_tx(id).is_some(),
            "touched tx must not be reaped"
        );
    }
}
