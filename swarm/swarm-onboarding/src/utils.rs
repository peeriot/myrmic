//! Utility functions and types for the onboarding crate

use embedded_io_async::{Error, ErrorKind};

pub mod base38;
pub mod base64;
pub mod future;
pub mod io;
pub mod slicebuf;

/// A generic buffer overflow error
/// Used by the `slicebuf` and `io` modules
#[derive(thiserror::Error, Debug, Copy, Clone, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[error("Buffer overflow error")]
pub struct BufferOverflowError;

impl Error for BufferOverflowError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::OutOfMemory
    }
}
