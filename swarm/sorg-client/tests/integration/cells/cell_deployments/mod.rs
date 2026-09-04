mod deploy;
mod undeploy;

use std::time::Duration;

use sorg_client::Client as SorgClient;
use sorg_tests::{TestApp, build_and_register_cell_class, swarm_config};

use crate::integration::spawn_full_test_app_with_swarm;

const CELL_SRI: &str = "test-deploy-cell";
const CELL_CLASS: &str = "dummy";

/// Full stored key for the marker `dummy_cell`'s `output` command writes:
/// `Kv::new("dummy")` + relative key `"output"`.
const DUMMY_MARKER_KEY: &str = "dummy/output";

async fn spawn_test_app_with_dummy_cell() -> (TestApp, SorgClient) {
    let swarm = swarm_config!("full.jsonnet");
    build_and_register_cell_class("../../tests/fixtures/dummy_cell", CELL_CLASS, &swarm).await;
    let test_app = spawn_full_test_app_with_swarm(swarm).await;
    let sorg = SorgClient::new(test_app.session().clone());
    (test_app, sorg)
}

/// Polls the deployed cell's private KV for the `output` marker, returning whether it
/// appears within ~2.5s. `output` is fire-and-forget, so the write lands asynchronously.
async fn output_marker_seen(test_app: &TestApp, sri: &str) -> bool {
    for _ in 0..25 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if test_app.read_cell_kv(sri, DUMMY_MARKER_KEY).await.is_some() {
            return true;
        }
    }
    false
}
