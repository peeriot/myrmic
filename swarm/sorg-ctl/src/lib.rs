use commands::Command;
pub use error::{Error, Result};
use sorg_client::Client;

mod commands;
mod error;
mod utils;

// pub(crate) use utils::get_random_orchestration_runtime;

#[derive(clap::Parser)]
pub struct Ctl {
    // TODO we may want to provide a zenoh config here to specify which runtime we connect to
    #[command(subcommand)]
    command: Command,
}

impl Ctl {
    pub async fn process_command(self, client: Client) -> Result<()> {
        match self.command.process(client).await {
            Ok(()) => Ok(()),
            Err(err) => {
                print_error!("{err}\n");
                std::process::exit(1)
            }
        }
    }
}
