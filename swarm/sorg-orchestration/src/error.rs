use zenoh::bytes::ZBytes;

pub type Result<T, E = Error> = core::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    SorgCommon(#[from] sorg_common::Error),

    /// A deploy or delete confirmation from an embedded runtime was not received
    /// within the configured deadline.
    #[error("{0}")]
    DeploymentTimeout(String),
}

impl From<Error> for ZBytes {
    fn from(value: Error) -> Self {
        let err_msg = value.to_string();
        err_msg.into()
    }
}
