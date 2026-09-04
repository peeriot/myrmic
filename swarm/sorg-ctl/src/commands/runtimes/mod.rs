use list::list_exec_runtimes;
use sorg_client::Client;

use crate::Result;

mod list;

#[derive(clap::Subcommand)]
pub(super) enum RuntimeCommand {
    /// Provides an overview of the capabilities of the sorg runtimes reachable by the CLI
    List,
}

impl RuntimeCommand {
    pub(super) async fn process(self, client: Client) -> Result<()> {
        match self {
            RuntimeCommand::List => list_exec_runtimes(client).await,
        }
    }
}
