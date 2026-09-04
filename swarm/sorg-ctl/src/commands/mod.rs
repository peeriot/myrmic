//! Module for the different commands that the CLI can serve

mod files;
mod orchestration;
mod runtimes;

use files::FilesCommand;
use orchestration::OrchestrationCommand;
use runtimes::RuntimeCommand;
use sorg_client::Client;

use crate::Result;

#[allow(private_interfaces)]
#[derive(clap::Subcommand)]
pub(super) enum Command {
    /// Offers subcommands to manage and query the state of reachable sorg runtimes (you can use the shorter 'rt')
    #[command(subcommand, alias = "rt")]
    Runtimes(RuntimeCommand),

    /// Offers subcommand to manage and query the reachable orchestration runtimes
    #[command(subcommand, alias = "or")]
    Orchestration(OrchestrationCommand),

    /// Offers subcommand to upload local files (specified via a file manifest) to the swarm filestore
    #[command(subcommand, alias = "fs")]
    Files(FilesCommand),
}

impl Command {
    pub(super) async fn process(self, client: Client) -> Result<()> {
        match self {
            Command::Runtimes(runtime_commands) => runtime_commands.process(client).await,
            Command::Orchestration(orchestration_command) => {
                orchestration_command.process(client).await
            }
            Command::Files(files_command) => files_command.process(client).await,
        }
    }
}
