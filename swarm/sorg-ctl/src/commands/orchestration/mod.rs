use list::list_orch_runtimes;
use sorg_client::Client;

use crate::Result;

mod list;

#[derive(clap::Subcommand)]
pub(super) enum OrchestrationCommand {
    /// Provides an overview of the capabilities of the reachable orchestration runtimes
    List,
}

impl OrchestrationCommand {
    pub(super) async fn process(self, client: Client) -> Result<()> {
        match self {
            OrchestrationCommand::List => list_orch_runtimes(client).await,
        }
    }
}
