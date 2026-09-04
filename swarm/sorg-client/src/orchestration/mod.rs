use sorg_common::{OrchRuntimeRecord, query_orch_runtimes};

use crate::{Client, Result};

impl Client {
    /// Asynchronously retrieves a list of orchestration runtime records describing the orchestration
    /// runtimes which are reachable via the client.
    pub async fn list_orch_runtimes(&self) -> Result<Vec<OrchRuntimeRecord>> {
        let orch_rt_records = query_orch_runtimes(self.session()).await?;
        Ok(orch_rt_records)
    }
}
