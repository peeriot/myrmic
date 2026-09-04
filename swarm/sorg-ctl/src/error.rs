use std::str::Utf8Error;

pub type Result<T, E = Error> = core::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    SorgError(#[from] sorg_client::types::SorgError),

    #[error(transparent)]
    SorgClient(#[from] sorg_client::Error),

    #[error(transparent)]
    FileStore(#[from] filestore_client::Error),

    // -- Externals
    #[error(transparent)]
    Fmt(#[from] core::fmt::Error),

    #[error(transparent)]
    Utf8Errr(#[from] Utf8Error),

    #[error(transparent)]
    SerdeYaml(#[from] serde_yaml::Error),

    #[error(transparent)]
    IoError(#[from] std::io::Error),
}
