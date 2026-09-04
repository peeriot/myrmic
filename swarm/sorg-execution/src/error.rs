use std::time::Duration;

pub type Result<T, E = Error> = core::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    SorgCommon(#[from] sorg_common::Error),

    #[error(transparent)]
    Mailbox(#[from] cell_mailbox::Error),

    #[error(transparent)]
    FileStore(#[from] filestore_client::Error),

    #[error(
        "The initialization of a task took longer than the configured timeout of '{timeout:?}'. Double check the init logic and/or consider setting a higher value for the 'init_timeout_secs' setting of the configuration of the execution plugin."
    )]
    InitTimeout { timeout: Duration },

    // -- Externals
    #[error(transparent)]
    Wasmtime(#[from] wasmtime::Error),

    #[error(transparent)]
    RumqttcConnection(#[from] rumqttc::ConnectionError),

    #[error(transparent)]
    RumqttcClient(#[from] rumqttc::ClientError),
}
