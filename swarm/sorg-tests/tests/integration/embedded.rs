//! Tests for [`MockEmbeddedExec`], the mock embedded execution runtime offered
//! by `sorg-tests`.
//!
//! Each test plays the orchestrator by hand against the datalayer — writing a
//! `DeploymentCommand` to the mock's deployment mailbox and polling for the
//! `DeploymentConfirmation` — to exercise the mock under each response mode.

use std::time::Duration;

use cell_protocol::{
    DEPLOYMENT_RESPONSES_TABLE, DEPLOYMENT_TABLE, DeploymentCommand, DeploymentConfirmation,
    RuntimeId, Sri, scope_of_deployment,
};
use db_client::v1::Client as DbClient;
use db_client::v1::models::{tb_insert, tb_list};
use myrmic_tags::Platform;
use sorg_tests::{DeployResponseMode, MockEmbeddedExec, TestApp, swarm_config};

const POLL_INTERVAL: Duration = Duration::from_millis(200);
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(5);
const SILENT_WAIT: Duration = Duration::from_secs(2);

const CELL_SRI: &str = "embedded-cell-1";
const CELL_CLASS: &str = "embedded-cell-1";

/// In `ConfirmSuccess` mode the mock registers with the embedded tags and
/// replies with a `DeploymentConfirmation` carrying no failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn mock_embedded_exec_confirms_success() {
    // Arrange - bring up a swarm with a DB and a mock embedded node that
    // confirms deployments successfully.
    let swarm = swarm_config!("db.jsonnet");
    let test_app = TestApp::spawn(swarm, || async { true }).await;
    let db = DbClient::new(test_app.session());
    let mock = MockEmbeddedExec::spawn(Platform::Esp32c6, DeployResponseMode::ConfirmSuccess).await;
    let exec_id = mock.id();

    // Act - play the orchestrator: write a DeploymentCommand into the mock's mailbox.
    let sri = Sri::from_target(CELL_SRI).unwrap();
    write_deployment_command(&db, exec_id, &sri).await;

    // Assert - the mock registered with the embedded tags, confirmed the
    // deployment, and recorded the command it consumed.
    assert_registered_with_embedded_tags(&test_app, exec_id).await;

    let confirmation = wait_for_confirmation(
        &db,
        exec_id,
        CONFIRM_TIMEOUT,
        |c| matches!(c, DeploymentConfirmation::Deployed { sri: s, .. } if *s == sri),
    )
    .await
    .expect("expected a deployment confirmation from the mock, got none before timeout");
    let DeploymentConfirmation::Deployed { failure, .. } = &confirmation else {
        panic!("expected a Deployed confirmation, got {confirmation:?}");
    };
    assert!(
        failure.is_none(),
        "expected a success confirmation, got failure: {failure:?}",
    );

    let received = mock.received_commands();
    assert_eq!(
        received.len(),
        1,
        "mock should have consumed exactly one deployment command"
    );
    let DeploymentCommand::Deploy { sri: cmd_sri, .. } = &received[0] else {
        panic!("expected a Deploy command, got {:?}", received[0]);
    };
    assert_eq!(
        cmd_sri, &sri,
        "mock consumed a deployment command for an unexpected SRI"
    );
}

/// In `ConfirmFailure` mode the mock replies with a confirmation carrying the
/// configured failure message.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn mock_embedded_exec_reports_failure() {
    // Arrange - a mock embedded node configured to fail deployments.
    let failure_msg = "deployment rejected: out of flash";
    let swarm = swarm_config!("db.jsonnet");
    let test_app = TestApp::spawn(swarm, || async { true }).await;
    let db = DbClient::new(test_app.session());
    let mock = MockEmbeddedExec::spawn(
        Platform::Esp32c6,
        DeployResponseMode::ConfirmFailure(failure_msg.to_owned()),
    )
    .await;
    let exec_id = mock.id();

    // Act - play the orchestrator: write a DeploymentCommand into the mock's mailbox.
    let sri = Sri::from_target(CELL_SRI).unwrap();
    write_deployment_command(&db, exec_id, &sri).await;

    // Assert - the mock confirms with the configured failure message.
    let confirmation = wait_for_confirmation(
        &db,
        exec_id,
        CONFIRM_TIMEOUT,
        |c| matches!(c, DeploymentConfirmation::Deployed { sri: s, .. } if *s == sri),
    )
    .await
    .expect("expected a deployment confirmation from the mock, got none before timeout");
    let DeploymentConfirmation::Deployed { failure, .. } = &confirmation else {
        panic!("expected a Deployed confirmation, got {confirmation:?}");
    };
    assert_eq!(
        failure.as_deref(),
        Some(failure_msg),
        "expected the confirmation to carry the configured failure message"
    );
}

/// In `Silent` mode the mock consumes the command but never replies, so the
/// orchestrator would time out waiting for a confirmation.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn mock_embedded_exec_stays_silent() {
    // Arrange - a mock embedded node configured to stay silent.
    let swarm = swarm_config!("db.jsonnet");
    let test_app = TestApp::spawn(swarm, || async { true }).await;
    let db = DbClient::new(test_app.session());
    let mock = MockEmbeddedExec::spawn(Platform::Esp32c6, DeployResponseMode::Silent).await;
    let exec_id = mock.id();

    // Act - play the orchestrator: write a DeploymentCommand into the mock's mailbox.
    let sri = Sri::from_target(CELL_SRI).unwrap();
    write_deployment_command(&db, exec_id, &sri).await;

    // Assert - the mock consumes the command but never confirms it.
    assert!(
        wait_until_consumed(&mock, &sri, CONFIRM_TIMEOUT).await,
        "mock never consumed the deployment command"
    );
    let confirmation = wait_for_confirmation(
        &db,
        exec_id,
        SILENT_WAIT,
        |c| matches!(c, DeploymentConfirmation::Deployed { sri: s, .. } if *s == sri),
    )
    .await;
    assert!(
        confirmation.is_none(),
        "expected no confirmation in silent mode, got {confirmation:?}"
    );
}

/// The mock confirms a `DeploymentCommand::Delete` with a
/// `DeploymentConfirmation::Deleted` and records the command.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn mock_embedded_exec_confirms_delete() {
    // Arrange — bring up a swarm with a mock embedded node.
    let swarm = swarm_config!("db.jsonnet");
    let test_app = TestApp::spawn(swarm, || async { true }).await;
    let db = DbClient::new(test_app.session());
    let mock = MockEmbeddedExec::spawn(Platform::Esp32c6, DeployResponseMode::ConfirmSuccess).await;
    let exec_id = mock.id();

    // Act — play the orchestrator: write a Delete command into the mock's mailbox.
    let sri = Sri::from_target("test-cell").unwrap();
    write_delete_command(&db, exec_id, sri).await;

    // Assert — the mock confirmed the deletion and recorded the command.
    let confirmation = wait_for_confirmation(&db, exec_id, CONFIRM_TIMEOUT, |c| {
        matches!(c, DeploymentConfirmation::Deleted { .. })
    })
    .await
    .expect("expected a Deleted confirmation from the mock, got none before timeout");
    assert!(
        matches!(confirmation, DeploymentConfirmation::Deleted { sri: ref s } if *s == sri),
        "expected a Deleted confirmation with sri {sri:?}, got {confirmation:?}"
    );

    let received = mock.received_commands();
    assert_eq!(
        received.len(),
        1,
        "mock should have consumed exactly one command"
    );
    assert!(
        matches!(&received[0], DeploymentCommand::Delete { sri: s } if *s == sri),
        "expected a Delete command with sri {sri:?}, got {:?}",
        received[0]
    );
}

/// Asserts the runtime registered under `exec_id` carries all of the esp32c6
/// capability tags.
async fn assert_registered_with_embedded_tags(test_app: &TestApp, exec_id: RuntimeId) {
    let execs = test_app.list_registered_execs().await;
    let entry = execs
        .into_iter()
        .find(|e| e.id() == exec_id)
        .expect("mock embedded exec is not present in the registry");
    let registered: Vec<&str> = entry
        .capabilities()
        .tags()
        .iter()
        .map(std::convert::AsRef::as_ref)
        .collect();
    for tag in Platform::Esp32c6.get_tags() {
        assert!(
            registered.contains(&tag),
            "registered exec is missing the embedded tag `{tag}`: {registered:?}"
        );
    }
}

/// Inserts a `DeploymentCommand` into the mock's deployment table, mimicking
/// the orchestrator's side of the deployment protocol.
async fn write_deployment_command(db: &DbClient, exec_id: RuntimeId, sri: &Sri) {
    let command = DeploymentCommand::Deploy {
        class: CELL_CLASS.to_owned(),
        sri: *sri,
        payload: None,
        gen_id: cell_protocol::Gen::from_parts(1, 1),
        lineage: cell_protocol::SpawnLineage::default(),
    };
    let value = postcard::to_allocvec(&command).expect("serialize deployment command");
    db.write_tx_in(scope_of_deployment(exec_id), async move |client, tx_id| {
        client
            .send(tb_insert::Request {
                id: tx_id,
                op: tb_insert::Op {
                    scope: scope_of_deployment(exec_id),
                    table: DEPLOYMENT_TABLE.to_owned(),
                    value,
                    eid: None,
                },
            })
            .await
            .expect("send insert request")
            .expect("insert deployment command");
        Ok(())
    })
    .await
    .expect("write deployment command transaction");
}

/// Inserts a `DeploymentCommand::Delete` into the mock's deployment table.
async fn write_delete_command(db: &DbClient, exec_id: RuntimeId, sri: Sri) {
    let command = DeploymentCommand::Delete { sri };
    let value = postcard::to_allocvec(&command).expect("serialize delete command");
    db.write_tx_in(scope_of_deployment(exec_id), async move |client, tx_id| {
        client
            .send(tb_insert::Request {
                id: tx_id,
                op: tb_insert::Op {
                    scope: scope_of_deployment(exec_id),
                    table: DEPLOYMENT_TABLE.to_owned(),
                    value,
                    eid: None,
                },
            })
            .await
            .expect("send insert request")
            .expect("insert delete command");
        Ok(())
    })
    .await
    .expect("write delete command transaction");
}

/// Polls the mock's responses table until a confirmation matching `predicate`
/// appears or `timeout` elapses. Filtering happens outside the read-tx so the
/// predicate does not need to be `Clone` or `'static`.
async fn wait_for_confirmation(
    db: &DbClient,
    exec_id: RuntimeId,
    timeout: Duration,
    predicate: impl Fn(&DeploymentConfirmation) -> bool,
) -> Option<DeploymentConfirmation> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let confirmations = db
            .read_tx_in(scope_of_deployment(exec_id), async move |client, tx_id| {
                let tb_list::Response { entities } = client
                    .send(tb_list::Request {
                        id: tx_id,
                        op: tb_list::Op {
                            scope: scope_of_deployment(exec_id),
                            table: DEPLOYMENT_RESPONSES_TABLE.to_owned(),
                            cursor: None,
                            limit: None,
                            order: None,
                        },
                    })
                    .await
                    .expect("send list request")
                    .expect("list deployment responses");
                Ok(entities
                    .into_iter()
                    .filter_map(|(_id, value)| {
                        postcard::from_bytes::<DeploymentConfirmation>(&value).ok()
                    })
                    .collect::<Vec<_>>())
            })
            .await
            .expect("read deployment responses transaction");

        if let Some(found) = confirmations.into_iter().find(|c| predicate(c)) {
            return Some(found);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Polls until the mock has consumed a command for `sri`, or `timeout` elapses.
async fn wait_until_consumed(mock: &MockEmbeddedExec, sri: &Sri, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if mock
            .received_commands()
            .iter()
            .any(|c| matches!(c, DeploymentCommand::Deploy { sri: s, .. } if s == sri))
        {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
