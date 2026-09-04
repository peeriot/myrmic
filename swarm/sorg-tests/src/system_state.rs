use std::time::{Duration, Instant};

use crate::TestApp;

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const POLL_TIMEOUT: Duration = Duration::from_secs(3);

impl TestApp {
    pub async fn wait_for_registered_exec(&self, id_prefix: &str) {
        let start = Instant::now();
        loop {
            if let Ok(execs) = self.sorg_client.list_registered_execs().await
                && execs
                    .iter()
                    .any(|e| e.id().to_string().starts_with(id_prefix))
            {
                return;
            }
            assert!(
                start.elapsed() < POLL_TIMEOUT,
                "timed out waiting for exec {id_prefix} to appear in registry after {POLL_TIMEOUT:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    pub async fn wait_for_deregistered_exec(&self, id_prefix: &str) {
        let start = Instant::now();
        loop {
            if let Ok(execs) = self.sorg_client.list_registered_execs().await
                && !execs
                    .iter()
                    .any(|e| e.id().to_string().starts_with(id_prefix))
            {
                return;
            }
            assert!(
                start.elapsed() < POLL_TIMEOUT,
                "timed out waiting for exec {id_prefix} to leave registry after {POLL_TIMEOUT:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}
