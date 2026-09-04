//! deployable binary for the test-sidecar server, see `test_framework::sidecar::server`
#[tokio::main]
async fn main() {
    test_framework::sidecar::server::run().await;
}
