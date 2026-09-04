use std::time::{Duration, Instant};

use claims::assert_ok;
use sorg_common::ExecRuntimeInfo;

use crate::TestApp;

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const POLL_TIMEOUT: Duration = Duration::from_secs(3);

impl TestApp {
    /// Lists the execution runtimes currently registered in the exec registry.
    pub async fn list_registered_execs(&self) -> Vec<ExecRuntimeInfo> {
        assert_ok!(self.sorg_client.list_registered_execs().await)
    }

    pub async fn orch_rt_present(&self, id_prefix: &str) -> bool {
        let start = Instant::now();
        loop {
            if let Ok(rts) = self.sorg_client.list_orch_runtimes().await
                && rts
                    .iter()
                    .any(|rt| rt.id.to_string().starts_with(id_prefix))
            {
                return true;
            }
            if start.elapsed() >= POLL_TIMEOUT {
                return false;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    pub async fn orch_rt_absent(&self, id_prefix: &str) -> bool {
        let start = Instant::now();
        loop {
            if let Ok(rts) = self.sorg_client.list_orch_runtimes().await
                && !rts
                    .iter()
                    .any(|rt| rt.id.to_string().starts_with(id_prefix))
            {
                return true;
            }
            if start.elapsed() >= POLL_TIMEOUT {
                return false;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}
