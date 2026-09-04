use core::str;
use std::fmt::{Debug, Display};

use zenoh::{bytes::ZBytes, query::ReplyError};

use crate::{ClientSendError, ZenohError};

pub type Result<T, E = Error> = core::result::Result<T, E>;

pub fn query_err_payload<E: ToString>(err: &E) -> ZBytes {
    let err_msg = err.to_string();
    ZBytes::from(err_msg)
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Custom(String),

    /// Returned when processing invalid content provided by the user. Wraps a message describing why the input
    /// is invalid
    #[error("Validation Error: {0}")]
    Validation(String),

    /// Returned when a sender fails to send an event to an event loop. Most likely indicates that the event loop
    /// terminated unexpectedly
    #[error(
        "Failed to send event '{event_desc}' to an event loop. Event loop seems to have terminated unexpectedly."
    )]
    EventSendFailure { event_desc: String },

    /// Returned when an interaction with Zenoh fails. Provides the context of the zenoh interaction as well as the error
    /// returned by zenoh
    #[error("Error when using zenoh to {context}: {error}")]
    Zenoh {
        context: String,
        #[source]
        error: ZenohError,
    },

    // External errors
    #[error("Postcard error in the context '{context}'; Error message: {error}")]
    Postcard {
        context: &'static str,
        #[source]
        error: postcard::Error,
    },

    #[error(transparent)]
    SerdeYaml(#[from] serde_yaml::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Error {
    pub fn custom(val: impl Display) -> Self {
        Self::Custom(val.to_string())
    }

    pub fn validation(val: impl Display) -> Self {
        Self::Validation(val.to_string())
    }

    pub fn zenoh(context: impl Display, zenoh_err: ZenohError) -> Self {
        Self::Zenoh {
            context: context.to_string(),
            error: zenoh_err,
        }
    }
}

impl From<&str> for Error {
    fn from(val: &str) -> Self {
        Self::Custom(val.to_string())
    }
}

impl From<cell_mailbox::Error> for Error {
    fn from(err: cell_mailbox::Error) -> Self {
        Self::Custom(err.to_string())
    }
}

#[macro_export]
macro_rules! custom_err {
    ($($arg:tt)*) => {
        $crate::Error::custom(format!($($arg)*))
    };
}

#[macro_export]
macro_rules! bail {
    ($($arg:tt)*) => {
        return Err($crate::custom_err!($($arg)*).into())
    };
}

#[macro_export]
macro_rules! zenoh_err {
    ($context:literal, $zenoh_err: expr) => {
        $crate::Error::zenoh(format!($context), $zenoh_err)
    };
}

#[macro_export]
macro_rules! validation_err {
    ($($arg:tt)*) => {
        $crate::Error::validation(format!($($arg)*))
    };
}

#[macro_export]
macro_rules! bail_validation {
    ($($arg:tt)*) => {
        return Err($crate::validation_err!($($arg)*))
    };
}

impl<T> From<ClientSendError<T>> for Error
where
    T: Debug,
{
    fn from(value: ClientSendError<T>) -> Self {
        let event_desc = format!("{event:?}", event = value.0);
        Error::EventSendFailure { event_desc }
    }
}

#[must_use]
pub fn is_query_timeout(err: &ReplyError) -> bool {
    if let Ok(msg) = str::from_utf8(&err.payload().to_bytes()) {
        msg == "Timeout"
    } else {
        false
    }
}

#[cfg(test)]
mod test {
    use super::Error;

    #[test]
    fn zenoh_macro_works() {
        let zenoh_err = Box::new(Error::custom("bla"));
        let topic = "my topic";
        let my_zenoh_err = zenoh_err!("error regarding topic {topic}", zenoh_err);
        let expected = "error regarding topic my topic";
        let Error::Zenoh { context, error: _ } = my_zenoh_err else {
            panic!("weird")
        };
        assert_eq!(expected, context);
    }
}
