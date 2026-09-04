pub type Result<T, E = Error> = core::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    SorgCommon(#[from] sorg_common::Error),

    #[error(transparent)]
    IoError(#[from] std::io::Error),
}
