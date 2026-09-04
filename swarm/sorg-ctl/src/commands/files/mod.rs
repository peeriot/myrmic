use std::path::PathBuf;

use filestore_client::Client;
use manifest::read_file_manifest;
use sorg_client::Client as SorgClient;

use crate::Result;

mod manifest;

#[derive(clap::Subcommand)]
pub(super) enum FilesCommand {
    /// Stores local files in the filestore, according to a provided files manifest
    Store {
        /// Path to the files manifest
        #[arg(value_name = "MANIFEST_FILE")]
        manifest_file: PathBuf,
    },
}

impl FilesCommand {
    pub(super) async fn process(self, client: SorgClient) -> Result<()> {
        match self {
            FilesCommand::Store { manifest_file } => {
                let fs_client = Client::new(client.session());
                store_local_files(fs_client, manifest_file).await
            }
        }
    }
}

pub(crate) async fn store_local_files(client: Client, file_manifest_path: PathBuf) -> Result<()> {
    let files = read_file_manifest(file_manifest_path)?;

    for file in files {
        let bytes = std::fs::read(file.local_path)?;
        client.store_file(file.fs_path.as_ref(), bytes).await?;
    }

    Ok(())
}
