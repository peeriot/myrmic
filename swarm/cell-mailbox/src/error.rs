//! The mailbox's error type. Self-contained so the crate needn't depend on
//! `sorg-common` (which would create a cycle with its re-export).

use core::fmt::Display;

/// Errors produced while sending or receiving cell messages.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The db request could not be delivered (transport/zenoh failure).
    #[error("{context}: unable to communicate with db: {message}")]
    Comm {
        context: &'static str,
        message: String,
    },
    /// The db processed the request but returned an error.
    #[error("{context}: db error: {message}")]
    Db {
        context: &'static str,
        message: String,
    },
    /// A payload failed to (de)serialize.
    #[error("{context}: (de)serialization failed: {source}")]
    Serde {
        context: &'static str,
        #[source]
        source: postcard::Error,
    },
}

impl Error {
    pub(crate) fn comm(context: &'static str, err: impl Display) -> Self {
        Self::Comm {
            context,
            message: err.to_string(),
        }
    }

    pub(crate) fn db(context: &'static str, message: String) -> Self {
        Self::Db { context, message }
    }

    pub(crate) fn serde(context: &'static str, source: postcard::Error) -> Self {
        Self::Serde { context, source }
    }
}

pub type Result<T> = core::result::Result<T, Error>;

pub(crate) fn to_bytes<T: serde::Serialize>(value: &T, context: &'static str) -> Result<Vec<u8>> {
    postcard::to_allocvec(value).map_err(|err| Error::serde(context, err))
}

pub(crate) fn from_bytes<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
    context: &'static str,
) -> Result<T> {
    postcard::from_bytes(bytes).map_err(|err| Error::serde(context, err))
}
