use std::str::Utf8Error;

pub type Result<T, E = Error> = core::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    TestControlCommon(#[from] test_control_common::Error),

    #[error(transparent)]
    Utf8Errr(#[from] Utf8Error),
}
