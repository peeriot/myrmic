//! Bundle reading and writing abstractions and implementations.

use embedded_io_async::{ErrorType, Read};

pub mod cbin;
pub mod tar;

/// A trait representing a reader for a bundle and abstracting bundle consumption code from
/// the actual bundle format (e.g., reading a tar archive, reading a zip file, etc).
///
/// A bundle is a streamable content representing a collection of 0 or more BLOBs,
/// where each BLOB is identified by a name.
pub trait BundleRead: ErrorType {
    /// A reader type for reading the contents of an item in the bundle.
    type Read<'a>: Read<Error = Self::Error>
    where
        Self: 'a;

    /// Read the next item from the bundle.
    ///
    /// # Returns
    /// - `Ok(Some((name, reader)))`: The next item was read successfully.
    /// - `Ok(None)`: There are no more items to read.
    /// - `Err(e)`: An error occurred while reading the next item.
    async fn next_item(&mut self) -> Result<Option<(&str, Self::Read<'_>)>, Self::Error>;
}

impl<T> BundleRead for &mut T
where
    T: BundleRead,
{
    type Read<'a>
        = T::Read<'a>
    where
        Self: 'a;

    async fn next_item(&mut self) -> Result<Option<(&str, Self::Read<'_>)>, Self::Error> {
        (*self).next_item().await
    }
}

/// A trait representing a writer for a bundle and abstracting bundle creation code from
/// the actual bundle format (e.g., writing to a tar archive, writing to a zip file, etc).
///
/// A bundle is a streamable content representing a collection of 0 or more BLOBs,
/// where each BLOB is identified by a name.
pub trait BundleWrite: ErrorType {
    /// A writer for a single item in the bundle.
    type Write<'a>: zenoh_traits::Write<Error = Self::Error>
    where
        Self: 'a;

    /// Get the next item writer in the bundle.
    async fn next_item(&mut self, name: &str) -> Result<Self::Write<'_>, Self::Error>;

    /// Close the bundle writer.
    async fn close(&mut self) -> Result<(), Self::Error>;
}

impl<T> BundleWrite for &mut T
where
    T: BundleWrite,
{
    type Write<'a>
        = T::Write<'a>
    where
        Self: 'a;

    async fn next_item(&mut self, name: &str) -> Result<Self::Write<'_>, Self::Error> {
        (*self).next_item(name).await
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        (*self).close().await
    }
}
