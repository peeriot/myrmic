use sorg_common::{ExecRuntimeInfo, query_exec_runtimes};

use crate::{Client, Result};

impl Client {
    /// Asynchronously retrieves a list of execution runtime records describing the execution
    /// runtimes which are reachable via the client.
    pub async fn list_exec_runtimes(&self) -> Result<Vec<ExecRuntimeInfo>> {
        let exec_records = query_exec_runtimes(self.session()).await?;
        Ok(exec_records)
    }

    pub async fn list_registered_execs(&self) -> Result<Vec<ExecRuntimeInfo>> {
        let execs = sorg_common::exec_registry::list_registered_execs(self.session()).await?;
        Ok(execs)
    }
}
