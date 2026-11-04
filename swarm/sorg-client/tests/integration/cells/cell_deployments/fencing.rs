//! Generation fencing on the placement and instance rows: a mutation only ever
//! touches the incarnation it was issued for, so a stale deploy, rollback or
//! undeploy cannot disturb a successor that reused the SRI.

use cell_protocol::{CellInstance, Gen, PlacementEntry, PlacementKind, SpawnLineage, Sri};
use claims::{assert_err, assert_ok, assert_some};
use sorg_common::{
    FenceOutcome, PlacementClaimOutcome, claim_placement, commit_placement, get_placement,
    instance_registry, remove_placement,
};

use crate::integration::spawn_db_test_app;

const SRI: &str = "fenced-cell";
const FIRST: Gen = Gen::from_parts(1, 1);
const SECOND: Gen = Gen::from_parts(2, 1);

fn placeholder(sri: Sri, gen_id: Gen) -> PlacementEntry {
    PlacementEntry {
        sri,
        kind: PlacementKind::Placeholder,
        app: None,
        gen_id,
    }
}

fn instance(sri: Sri, gen_id: Gen) -> CellInstance {
    CellInstance {
        sri,
        class: cell_protocol::BlobHash::of(b"fenced"),
        gen_id,
        lineage: SpawnLineage::default(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn placement_mutations_are_fenced_by_generation() {
    let test_app = spawn_db_test_app().await;
    let session = test_app.session();
    let sri = Sri::from_target(SRI).unwrap();

    assert_eq!(
        assert_ok!(claim_placement(session, placeholder(sri, FIRST)).await),
        PlacementClaimOutcome::Claimed
    );

    // A deploy that lost the claim can neither publish itself...
    assert_eq!(
        assert_ok!(commit_placement(session, placeholder(sri, SECOND)).await),
        FenceOutcome::Superseded { current: FIRST }
    );
    // ...nor release the holder's row.
    assert_eq!(
        assert_ok!(remove_placement(session, &sri, SECOND).await),
        FenceOutcome::Superseded { current: FIRST }
    );
    assert_eq!(
        assert_some!(assert_ok!(get_placement(session, &sri).await)).gen_id,
        FIRST,
        "the holder's row should be untouched"
    );

    // The holder commits and releases its own row; a second release finds nothing.
    assert_eq!(
        assert_ok!(commit_placement(session, placeholder(sri, FIRST)).await),
        FenceOutcome::Applied
    );
    assert_eq!(
        assert_ok!(remove_placement(session, &sri, FIRST).await),
        FenceOutcome::Applied
    );
    assert_eq!(
        assert_ok!(remove_placement(session, &sri, FIRST).await),
        FenceOutcome::Absent
    );

    // A commit arriving after the undeploy does not resurrect the cell.
    assert_eq!(
        assert_ok!(commit_placement(session, placeholder(sri, FIRST)).await),
        FenceOutcome::Absent
    );
    assert!(
        assert_ok!(get_placement(session, &sri).await).is_none(),
        "a late commit must not write a row"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn instance_removal_is_fenced_by_generation() {
    let test_app = spawn_db_test_app().await;
    let session = test_app.session();
    let sri = Sri::from_target(SRI).unwrap();
    assert_ok!(instance_registry::insert_registry_entry(session, &instance(sri, FIRST)).await);
    assert_ok!(claim_placement(session, placeholder(sri, FIRST)).await);

    // Deployed under the same generation: the row is a live cell's record.
    assert_err!(
        instance_registry::remove_instance(session, &sri, FIRST).await,
        "removing a deployed incarnation's row should be refused"
    );

    // The incarnation is undeployed and a successor claims the SRI: the
    // successor is deployed now, so its row is refused the same way, while the
    // predecessor removes its own.
    assert_ok!(remove_placement(session, &sri, FIRST).await);
    assert_ok!(claim_placement(session, placeholder(sri, SECOND)).await);
    assert_err!(
        instance_registry::remove_instance(session, &sri, SECOND).await,
        "removing the deployed successor's row should be refused"
    );
    assert_eq!(
        assert_ok!(instance_registry::remove_instance(session, &sri, FIRST).await),
        FenceOutcome::Applied
    );
    assert_eq!(
        assert_ok!(instance_registry::remove_instance(session, &sri, FIRST).await),
        FenceOutcome::Absent
    );
}

/// Removing an older generation while a newer one exists removes it, without
/// touching the newer one, so nothing comes back once the newer one is removed
/// too.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn an_older_generation_is_removed_next_to_a_newer_one() {
    let test_app = spawn_db_test_app().await;
    let session = test_app.session();
    let sri = Sri::from_target(SRI).unwrap();
    // Two live generations, as after a merge: the newer one is written first,
    // so the older one's insert finds no older generation to retire.
    for gen_id in [SECOND, FIRST] {
        assert_ok!(instance_registry::insert_registry_entry(session, &instance(sri, gen_id)).await);
    }

    assert_eq!(
        assert_ok!(instance_registry::remove_instance(session, &sri, FIRST).await),
        FenceOutcome::Superseded { current: SECOND }
    );
    assert_eq!(
        assert_some!(assert_ok!(
            instance_registry::get_instance(session, &sri).await
        ))
        .gen_id,
        SECOND,
        "the newer generation should be untouched"
    );

    assert_eq!(
        assert_ok!(instance_registry::remove_instance(session, &sri, SECOND).await),
        FenceOutcome::Applied
    );
    assert!(
        assert_ok!(instance_registry::get_instance(session, &sri).await).is_none(),
        "the older generation must not come back"
    );
}
