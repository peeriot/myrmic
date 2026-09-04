use std::str::Utf8Error;

pub type Result<T, E = Error> = core::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    SorgCommon(#[from] sorg_common::Error),

    #[error(transparent)]
    Mailbox(#[from] cell_mailbox::Error),

    #[error(transparent)]
    Utf8Errr(#[from] Utf8Error),
}
