use zenoh::bytes::ZBytes;

pub type Result<T, E = Error> = core::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    SorgCommon(#[from] sorg_common::Error),

    /// Used to signal transient inconsistencies when sth which has already been deleted from the orch state is
    /// being referenced. An example is the case where an exec runtime reports an error with a deployment while this
    /// deployment is being deleted
    #[error("the {0} has already been deleted from the orchestrator state")]
    AlreadyDeleted(String),

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

pub(crate) fn deleted_element_err(element_description: String) -> Error {
    Error::AlreadyDeleted(element_description)
}
