use crate::domain::{Key, Scope, SyncMarker};
use crate::store::fjall::Transaction;
use crate::tests::{open_tmp, write};
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
