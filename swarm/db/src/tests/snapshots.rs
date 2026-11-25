use crate::domain::{FieldValue, Key, Scope, Snapshot, SyncMarker};
use crate::semantic::{Query, Update};
use crate::store::fjall::{Store, Transaction};
use crate::tests::{open_tmp, read, write};
use skey::StoreKey;

/// `(mutations, deletions)` sync-point counts for `scope`.
fn count_sync_points(tx: &Transaction, scope: Scope<'_>) -> (usize, usize) {
    let (lower, upper) = Key::sync_point().scope(scope).range().unwrap();

    let mut mutations = 0;
    let mut deletions = 0;
    tx.find_sync_points(lower, upper, |_sp, sm| {
        match sm.marker {
            SyncMarker::Mutation => mutations += 1,
            SyncMarker::Deletion => deletions += 1,
        }
        Ok(())
    })
    .unwrap();

    (mutations, deletions)
}

#[tokio::test(flavor = "current_thread")]
async fn test_snapshot() {
    const TABLE: &str = "entity";

    let store = open_tmp();

    let scope = Scope::default();

    // 5 changesets (A -> B -> C -> D -> E), each inserting 2 unique rows.
    // So: 10 rows, 5 sync-points.
    for _ in 0..5 {
        let mut tx = write(&store);

        for i in 0..2 {
            let eid = uuid::Uuid::now_v7();
            let eid = eid.as_bytes().as_slice();

            tx.tb_insert(scope.table(TABLE), eid, format!("{i}").as_bytes())
                .expect("unable to insert row");
        }

        tx.commit().expect("unable to commit");
    }

    let snapshot_1 = {
        let mut tx = write(&store);
        let snapshot = tx.take_snapshot(scope).expect("unable to take snapshot");

        assert_eq!(
            tx.tb_count(scope.table(TABLE)).expect("unable to count"),
            10,
            "all 5 changesets should be present"
        );
        assert_eq!(
            count_sync_points(&tx, scope),
            (5, 0),
            "5 mutation sync-points, no deletions"
        );

        tx.rollback();
        snapshot
    };

    // Delete the data behind the last 3 sync-points (C, D, E).
    {
        let mut tx = write(&store);

        let sp_key = Key::sync_point().scope(scope);
        let (lower, upper) = sp_key.range().unwrap();

        let mut points = vec![];
        let mut seen = 0;
        tx.find_sync_points(lower, upper, |sp, _sm| {
            seen += 1;
            if seen > 2 {
                points.push(sp.as_id());
            }
            Ok(())
        })
        .unwrap();

        for point in points {
            tx.delete_chunk(sp_key.with_sp_id(point))
                .expect("unable to delete chunk");
        }

        tx.commit().expect("unable to commit");
    }

    {
        let mut tx = write(&store);

        assert_eq!(
            tx.tb_count(scope.table(TABLE)).expect("unable to count"),
            4,
            "only A and B rows should remain"
        );
        // `delete_chunk` only removes the rows; the sync-points themselves stay put
        // (unlike surrealkv's `erase_chunk`, which records a deletion marker).
        assert_eq!(count_sync_points(&tx, scope), (5, 0));

        tx.rollback();
    }

    // Restore the pre-delete snapshot.
    {
        let mut tx = write(&store);

        tx.restore_snapshot(scope, snapshot_1)
            .expect("unable to restore snapshot");

        tx.commit().expect("unable to commit");
    }

    let mut tx = write(&store);

    // Everything should be back, with no leftover dead rows or sync-points.
    assert_eq!(
        tx.tb_count(scope.table(TABLE)).expect("unable to count"),
        10,
        "restore should bring back all rows"
    );
    assert_eq!(
        count_sync_points(&tx, scope),
        (5, 0),
        "restore should leave exactly the snapshot's sync-points"
    );
}

/// One data kind of the restore matrix: writes a value into `scope` and
/// reports whether it reads back there.
struct Kind {
    name: &'static str,
    write: fn(&mut Transaction, Scope<'_>),
    present: fn(&mut Transaction, Scope<'_>) -> bool,
}

fn kinds() -> Vec<Kind> {
    vec![
        Kind {
            name: "kv",
            write: |tx, scope| tx.key_put(scope.kv("k"), b"v").expect("unable to put"),
            present: |tx, scope| {
                tx.key_get(scope.kv("k")).expect("unable to get").as_deref() == Some(&b"v"[..])
            },
        },
        Kind {
            name: "table",
            write: |tx, scope| {
                tx.tb_insert(scope.table("t"), b"id", b"v")
                    .expect("unable to insert");
            },
            present: |tx, scope| {
                tx.tb_get(scope.table("t"), b"id")
                    .expect("unable to get")
                    .as_deref()
                    == Some(&b"v"[..])
            },
        },
        Kind {
            name: "measurement",
            write: |tx, scope| {
                tx.publish_measurement(
                    scope,
                    "cpu",
                    vec![],
                    vec![(String::from("value"), FieldValue::F64(1.0))],
                    1,
                )
                .expect("unable to publish");
            },
            present: |tx, scope| {
                tx.find_measurement(scope, "cpu", None, None, None, None)
                    .expect("unable to find")
                    .len()
                    == 1
            },
        },
        Kind {
            name: "blob",
            write: |tx, scope| {
                let id = tx.store_blob(scope, b"blob").expect("unable to store");
                tx.link_blob(scope.path("/f"), id).expect("unable to link");
            },
            present: |tx, scope| {
                tx.resolve_path(scope.path("/f"))
                    .expect("unable to resolve")
                    .is_some_and(|(blob, _id)| blob.as_slice() == b"blob")
            },
        },
        Kind {
            name: "semantic",
            write: |tx, scope| {
                let insert =
                    Update::parse(r#"INSERT DATA { <http://ex/s> <http://ex/p> "o" }"#, None)
                        .expect("unable to parse update");
                tx.sem_update(scope, insert).expect("unable to update");
            },
            present: |tx, scope| {
                let ask = Query::parse(r#"ASK { <http://ex/s> <http://ex/p> "o" }"#, None)
                    .expect("unable to parse query");
                tx.sem_ask(scope, ask).expect("unable to ask")
            },
        },
    ]
}

fn write_kind(store: &Store, kind: &Kind, scope: Scope<'_>) {
    let mut tx = write(store);
    (kind.write)(&mut tx, scope);
    tx.commit().expect("unable to commit");
}

fn present(store: &Store, kind: &Kind, scope: Scope<'_>) -> bool {
    let mut tx = read(store);
    (kind.present)(&mut tx, scope)
}

fn snapshot_of(store: &Store, scope: Scope<'_>) -> Snapshot {
    read(store)
        .take_snapshot(scope)
        .expect("unable to take snapshot")
}

fn restore(store: &Store, scope: Scope<'_>, snapshot: Snapshot) {
    let mut tx = write(store);
    tx.restore_snapshot(scope, snapshot)
        .expect("unable to restore");
    tx.commit().expect("unable to commit");
}

/// Restore placement (swarm#1580): every data kind lands under the scope the
/// restore was asked for, and nowhere else.
#[tokio::test(flavor = "current_thread")]
async fn restore_into_the_same_scope_keeps_every_kind() {
    let src = Key::new_scope("d", "db", "src");

    for kind in kinds() {
        let store = open_tmp();
        write_kind(&store, &kind, src);
        let snapshot = snapshot_of(&store, src);

        restore(&store, src, snapshot);

        assert!(
            present(&store, &kind, src),
            "{}: readable after a same-scope restore",
            kind.name
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn restore_into_another_scope_moves_every_kind() {
    let src = Key::new_scope("d", "db", "src");
    let dst = Key::new_scope("d", "db", "dst");

    for kind in kinds() {
        let store = open_tmp();
        write_kind(&store, &kind, src);
        let snapshot = snapshot_of(&store, src);

        restore(&store, dst, snapshot);

        assert!(
            present(&store, &kind, dst),
            "{}: readable under the requested scope",
            kind.name
        );
        assert!(
            present(&store, &kind, src),
            "{}: the source scope keeps its data",
            kind.name
        );
        let tx = read(&store);
        assert_eq!(
            count_sync_points(&tx, src),
            (1, 0),
            "{}: the source sync point is untouched",
            kind.name
        );
        assert_eq!(
            count_sync_points(&tx, dst),
            (1, 0),
            "{}: one restored sync point",
            kind.name
        );
    }
}

/// Semantic data is left out: its term dictionary is global to the node and
/// travels in no snapshot, so it cannot be read on a fresh node yet.
#[tokio::test(flavor = "current_thread")]
async fn restore_onto_a_fresh_node_lands_in_the_requested_scope() {
    let src = Key::new_scope("d", "db", "src");
    let dst = Key::new_scope("d", "db", "dst");

    for kind in kinds().iter().filter(|kind| kind.name != "semantic") {
        let origin = open_tmp();
        write_kind(&origin, kind, src);
        let snapshot = snapshot_of(&origin, src);

        let fresh = open_tmp();
        restore(&fresh, dst, snapshot);

        assert!(
            present(&fresh, kind, dst),
            "{}: readable under the requested scope",
            kind.name
        );
        assert!(
            !present(&fresh, kind, src),
            "{}: nothing lands under the snapshot's own scope",
            kind.name
        );
        assert_eq!(
            count_sync_points(&read(&fresh), src),
            (0, 0),
            "{}: no sync point under the snapshot's own scope",
            kind.name
        );
    }
}
