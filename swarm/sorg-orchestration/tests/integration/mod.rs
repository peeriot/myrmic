#![allow(clippy::missing_panics_doc)]

use std::time::Duration;

use cell_protocol::Sri;
use sorg_common::{OrchRuntimeRecord, SorgPayload, TOPIC_ORCH_RUNTIMES};
use sorg_tests::{ScopedProcess, TestApp, swarm_config};
use zenoh::query::ConsolidationMode;

mod cells;
mod leader;

/// Resolves a test cell name (or UUID string) to its `Sri`, mirroring the
/// CLI/edge rule (`Sri::from_target`): a UUID literal is taken verbatim, any
/// other name is folded to its deterministic SRI.
pub fn to_sri(name: &str) -> Sri {
    Sri::from_target(name).expect("invalid cell SRI/name")
}

pub async fn spawn_empty_test_app() -> TestApp {
    let empty_swarm = swarm_config!("base_empty.jsonnet");
    TestApp::spawn(empty_swarm, || async { true }).await
}

pub async fn spawn_test_app_with_swarm(swarm: ScopedProcess) -> TestApp {
    let base_session = swarm.session().clone();
    let health_check = move || {
        let session = base_session.clone();
        async move {
            let replies = session
                .get(TOPIC_ORCH_RUNTIMES)
                .target(zenoh::query::QueryTarget::All)
                .consolidation(ConsolidationMode::None)
                .await
                .expect("failed to query orch runtimes");

            if let Ok(reply) = replies.recv_async().await {
                let sample = reply.into_result().unwrap();
                let _orch_record =
                    OrchRuntimeRecord::from_payload(sample.payload(), "deser orch record").unwrap();
                return true;
            }
            false
        }
    };

    let test_app = TestApp::spawn(swarm, health_check).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    test_app
}
