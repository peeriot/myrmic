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
async fn instance_erase_is_fenced_by_generation() {
    let test_app = spawn_db_test_app().await;
    let session = test_app.session();
    let sri = Sri::from_target(SRI).unwrap();
    let record = CellInstance {
        sri,
        class_name: "fenced".to_owned(),
        gen_id: FIRST,
        lineage: SpawnLineage::default(),
    };
    assert_ok!(instance_registry::insert_registry_entry(session, &record).await);
    assert_ok!(claim_placement(session, placeholder(sri, FIRST)).await);

    // Deployed under the same generation: the row is a live cell's record.
    assert_err!(
        instance_registry::erase_instance(session, &sri, FIRST).await,
        "erasing a deployed incarnation's row should be refused"
    );

    // The incarnation is undeployed and a successor claims the SRI: the
    // successor cannot erase the predecessor's row, but the predecessor can.
    assert_ok!(remove_placement(session, &sri, FIRST).await);
    assert_ok!(claim_placement(session, placeholder(sri, SECOND)).await);
    assert_eq!(
        assert_ok!(instance_registry::erase_instance(session, &sri, SECOND).await),
        FenceOutcome::Superseded { current: FIRST }
    );
    assert_eq!(
        assert_ok!(instance_registry::erase_instance(session, &sri, FIRST).await),
        FenceOutcome::Applied
    );
    assert_eq!(
        assert_ok!(instance_registry::erase_instance(session, &sri, FIRST).await),
        FenceOutcome::Absent
    );
}
