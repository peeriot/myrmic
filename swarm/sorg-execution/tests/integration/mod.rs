#![allow(clippy::missing_panics_doc)]

mod cells;
mod membership;
mod requirements;

use std::{str::FromStr, time::Duration};

use cell_protocol::RuntimeId;
use sorg_common::{ExecRuntimeInfo, SorgPayload, TOPIC_EXEC_RUNTIMES};
use sorg_tests::{ScopedProcess, TestApp};
use zenoh::{config::ZenohId, query::ConsolidationMode};

const BASE_ID_STRING: &str = "7728d1a01a04f41e7b9e0ff3bab594a2";

fn base_orch_id() -> RuntimeId {
    ZenohId::from_str(BASE_ID_STRING).unwrap().into()
}

pub async fn spawn_test_app_with_swarm(swarm: ScopedProcess) -> TestApp {
    let base_session = swarm.session().clone();
    let health_check = move || {
        let session = base_session.clone();
        async move {
            let replies = session
                .get(TOPIC_EXEC_RUNTIMES)
                .target(zenoh::query::QueryTarget::All)
                .consolidation(ConsolidationMode::None)
                .await
                .expect("failed to query orch runtimes");

            while let Ok(reply) = replies.recv_async().await {
                let sample = reply.into_result().unwrap();
                let exec_record =
                    ExecRuntimeInfo::from_payload(sample.payload(), "deser orch record").unwrap();
                if exec_record.id() == base_orch_id() {
                    return true;
                }
            }
            false
        }
    };

    let test_app = TestApp::spawn(swarm, health_check).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    test_app
}
