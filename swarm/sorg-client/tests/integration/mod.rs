use std::time::Duration;

use db_client::v1::Client as DbClient;
use sorg_common::{OrchRuntimeRecord, SorgPayload, TOPIC_ORCH_RUNTIMES};
use sorg_tests::{ScopedProcess, TestApp, swarm_config};
use zenoh::query::ConsolidationMode;

mod cells;

pub async fn spawn_db_test_app() -> TestApp {
    let swarm = swarm_config!("db_only.jsonnet");
    let session = swarm.session().clone();
    let health_check = move || {
        let session = session.clone();
        async move {
            let db = DbClient::new(&session);
            db.ping().await.is_ok()
        }
    };
    TestApp::spawn(swarm, health_check).await
}

pub async fn spawn_full_test_app_with_swarm(swarm: ScopedProcess) -> TestApp {
    let session = swarm.session().clone();
    let health_check = move || {
        let session = session.clone();
        async move {
            let replies = session
                .get(TOPIC_ORCH_RUNTIMES)
                .target(zenoh::query::QueryTarget::All)
                .consolidation(ConsolidationMode::None)
                .await
                .expect("failed to query orch runtimes");

            let Ok(reply) = replies.recv_async().await else {
                return false;
            };
            let Ok(sample) = reply.into_result() else {
                return false;
            };
            OrchRuntimeRecord::from_payload(sample.payload(), "deser orch record").is_ok()
        }
    };

    let test_app = TestApp::spawn(swarm, health_check).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    test_app
}
