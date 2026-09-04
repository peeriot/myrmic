//! A module providing implementations for reading and writing onboarding bundles as tar archives.
//!
//! TODO: Work in progress.

use zenoh_traits::{Error, ErrorKind, ErrorType, Read, Write};

use crate::bundle::{BundleRead, BundleWrite};

/// Errors that can occur during TAR reading or writing.
#[derive(thiserror::Error, Debug, Copy, Clone, Eq, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum TarError {
    /// An I/O error occurred.
    #[error("I/O error: {0}")]
    Io(#[from] ErrorKind),
}

impl TarError {
    /// Create a new `TarError::Io` from the given `ErrorKind`.
    pub fn io(kind: ErrorKind) -> Self {
        TarError::Io(kind)
    }
}

impl Error for TarError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Other
    }
}

/// An implementation of the `BundleRead` trait that reads the onboarding bundle as a tar archive.
pub struct TarRead<R>(R);

impl<R> ErrorType for TarRead<R> {
    type Error = TarError;
}

impl<R> BundleRead for TarRead<R>
where
    R: Read,
{
    type Read<'a>
        = TarBlobRead<R>
    where
        Self: 'a;

    async fn next_item(&mut self) -> Result<Option<(&str, Self::Read<'_>)>, Self::Error> {
        todo!()
    }
}

/// An implementation of the `BundleWrite` trait that writes the onboarding bundle as a tar archive.
pub struct TarWrite<W>(W);

impl<W> ErrorType for TarWrite<W> {
    type Error = TarError;
}

impl<W> BundleWrite for TarWrite<W>
where
    W: Write,
{
    type Write<'a>
        = TarBlobWrite<W>
    where
        Self: 'a;

    async fn next_item(&mut self, _name: &str) -> Result<Self::Write<'_>, Self::Error> {
        todo!()
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        todo!()
    }
}

/// A reader for a single blob within a tar archive.
pub struct TarBlobRead<R>(R);

impl<R> ErrorType for TarBlobRead<R> {
    type Error = TarError;
}

impl<R> Read for TarBlobRead<R>
where
    R: Read,
{
    async fn read(&mut self, _buf: &mut [u8]) -> Result<usize, Self::Error> {
        todo!()
    }
}

/// A writer for a single blob within a tar archive.
pub struct TarBlobWrite<W>(W);

impl<W> ErrorType for TarBlobWrite<W> {
    type Error = TarError;
}

impl<W> Write for TarBlobWrite<W>
where
    W: Write,
{
    async fn write(&mut self, _buf: &[u8]) -> Result<usize, Self::Error> {
        todo!()
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        todo!()
    }
}
