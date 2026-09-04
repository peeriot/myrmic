//! `BundleRead` and `BundleWrite` implementation for a very simple
//! "composite-binary" streamable data format.
//!
//! The format is a repetition of 0 or more blob elements, each having the following structure:
//! - 1 byte: length of the name of the BLOB
//! - N bytes: name of the BLOB (UTF-8 string as a sequence of bytes, no null-termination)
//! - M bytes: BLOB data (a sequence of bytes)
//!   The BLOB data is encoded in the following way:
//!   - The content of the BLOB is stored as-is except for the EOF byte (`0xf4`)
//!   - Each occurrence of the EOF byte _inside_ the BLOB content is escaped with itself, i.e. `0xf4 0xf4`
//!   - The end of the BLOB is always marked with the EOF byte
//!   - An empty BLOB is represented as a single EOF byte

use core::str::Utf8Error;

use embedded_io_async::BufRead;

use zenoh_traits::{Error, ErrorKind, ErrorType, Read, Write};

use crate::bundle::{BundleRead, BundleWrite};

/// The EOF byte
const EOF: u8 = 0xf4;

/// The maximum length of a BLOB name
const MAX_NAME_LEN: usize = 255;

/// Errors that can occur during CBin reading or writing.
#[derive(thiserror::Error, Debug, Copy, Clone, Eq, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum CBinError {
    /// The name of a BLOB is too long (more than 255 bytes).
    #[error("the name of a BLOB is too long")]
    NameTooLong,
    /// The name of a BLOB is not a valid UTF-8 string (when reading)
    #[error("the name of a BLOB is not valid UTF-8")]
    InvalidNameUtf8,
    /// The stream ended unexpectedly (when reading)
    #[error("the stream ended unexpectedly")]
    UnexpectedEof,
    /// An I/O error occurred.
    #[error("I/O error: {0}")]
    Io(#[from] ErrorKind),
}

impl CBinError {
    /// Create a new `CBinError::Io` from an I/O error.
    pub fn io<E: Error>(error: E) -> Self {
        Self::Io(error.kind())
    }
}

impl From<Utf8Error> for CBinError {
    fn from(_: Utf8Error) -> Self {
        Self::InvalidNameUtf8
    }
}

impl Error for CBinError {
    fn kind(&self) -> ErrorKind {
        match self {
            CBinError::NameTooLong => ErrorKind::InvalidInput,
            CBinError::InvalidNameUtf8 => ErrorKind::InvalidData,
            CBinError::UnexpectedEof => ErrorKind::InvalidInput,
            CBinError::Io(kind) => *kind,
        }
    }
}

/// A wrapper type for reading a CBin-formatted onboarding bundle from a stream.
pub struct CBinRead<'a, R> {
    read: R,
    buf: &'a mut [u8],
}

impl<'a, R: BufRead> CBinRead<'a, R> {
    /// Create a new `CBinRead` instance wrapping the given stream.
    ///
    /// # Arguments
    /// - `read`: The stream to read from.
    /// - `buf`: A buffer to use for reading names. Must be at least 255 bytes long
    ///
    /// # Panics
    /// - If `buf` is less than 255 bytes long.
    pub const fn new(read: R, buf: &'a mut [u8]) -> Self {
        assert!(buf.len() >= MAX_NAME_LEN);

        Self { read, buf }
    }
}

impl<R> ErrorType for CBinRead<'_, R> {
    type Error = CBinError;
}

impl<R: BufRead> BundleRead for CBinRead<'_, R> {
    type Read<'a>
        = CBinBlobRead<&'a mut R>
    where
        Self: 'a;

    async fn next_item(&mut self) -> Result<Option<(&str, Self::Read<'_>)>, Self::Error> {
        let data = self.read.fill_buf().await.map_err(CBinError::io)?;

        if data.is_empty() {
            // EOF
            return Ok(None);
        }

        let name_len = data[0] as usize;
        self.read.consume(1);

        if name_len > self.buf.len() {
            // Invalid name length
            return Err(CBinError::NameTooLong);
        }

        let mut offset = 0;
        while offset < name_len {
            let data = self.read.fill_buf().await.map_err(CBinError::io)?;
            if data.is_empty() {
                // EOF
                return Err(CBinError::UnexpectedEof);
            }

            let to_copy = core::cmp::min(name_len - offset, data.len());
            self.buf[offset..offset + to_copy].copy_from_slice(&data[..to_copy]);
            offset += to_copy;
            self.read.consume(to_copy);
        }

        Ok(Some((
            core::str::from_utf8(&self.buf[..name_len])?,
            CBinBlobRead {
                read: &mut self.read,
                eof: false,
            },
        )))
    }
}

/// A wrapper type for reading a CBin-formatted BLOB from a stream.
pub struct CBinBlobRead<R> {
    read: R,
    eof: bool,
}

impl<R> ErrorType for CBinBlobRead<R> {
    type Error = CBinError;
}

impl<R: BufRead> Read for CBinBlobRead<R> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let mut len = 0;

        loop {
            if buf.len() == len || self.eof {
                break;
            }

            let data = self.read.fill_buf().await.map_err(CBinError::io)?;
            if data.is_empty() {
                // EOF
                return Err(CBinError::UnexpectedEof);
            }

            if data[0] == EOF {
                // Check for escape byte
                self.read.consume(1);

                let data = self.read.fill_buf().await.map_err(CBinError::io)?;
                if data.is_empty() || data[0] != EOF {
                    // This was the EOF byte
                    self.eof = true;
                    break;
                }

                buf[len] = data[0];
            } else {
                buf[len] = data[0];
            }

            self.read.consume(1);
            len += 1;
        }

        Ok(len)
    }
}

/// A wrapper type for writing a CBin-formatted onboarding bundle to a stream.
pub struct CBinWrite<W: Write> {
    write: W,
    unfinished: bool,
}

impl<W> ErrorType for CBinWrite<W>
where
    W: Write,
{
    type Error = CBinError;
}

impl<W: Write> CBinWrite<W> {
    /// Create a new `CBinWrite` instance wrapping the given stream.
    ///
    /// # Arguments
    /// - `write`: The stream to write to.
    pub const fn new(write: W) -> Self {
        Self {
            write,
            unfinished: false,
        }
    }
}

impl<W: Write> BundleWrite for CBinWrite<W> {
    type Write<'a>
        = CBinBlobWrite<&'a mut W>
    where
        Self: 'a;

    async fn next_item(&mut self, name: &str) -> Result<Self::Write<'_>, Self::Error> {
        if self.unfinished {
            self.write.write(&[EOF]).await.map_err(CBinError::io)?;
            self.unfinished = false;
        }

        if name.len() > MAX_NAME_LEN {
            return Err(CBinError::NameTooLong);
        }

        self.write
            .write(&[name.len() as u8])
            .await
            .map_err(CBinError::io)?;
        self.write
            .write(name.as_bytes())
            .await
            .map_err(CBinError::io)?;
        self.unfinished = true;

        Ok(CBinBlobWrite(&mut self.write))
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        if self.unfinished {
            self.write.write(&[EOF]).await.map_err(CBinError::io)?;
            self.unfinished = false;
        }

        Ok(())
    }
}

impl<W> Drop for CBinWrite<W>
where
    W: Write,
{
    fn drop(&mut self) {
        if self.unfinished {
            warn!("CBinWrite dropped without being closed!");
            embassy_futures::block_on(self.write.write(&[EOF])).unwrap();
            self.unfinished = false;
        }
    }
}

/// A wrapper type for writing a CBin-formatted BLOB to a stream.
pub struct CBinBlobWrite<W>(W);

impl<W> ErrorType for CBinBlobWrite<W> {
    type Error = CBinError;
}

impl<W: Write> Write for CBinBlobWrite<W> {
    async fn write(&mut self, data: &[u8]) -> Result<usize, Self::Error> {
        if data.is_empty() {
            return Ok(0);
        }

        let mut written = 0;

        for &b in data {
            if b == EOF {
                // Escape byte
                self.0.write(&[EOF]).await.map_err(CBinError::io)?;
            }

            self.0.write(&[b]).await.map_err(CBinError::io)?;
            written += 1;
        }

        Ok(written)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.0.flush().await.map_err(CBinError::io)
    }
}

#[cfg(test)]
mod test {
    use zenoh_traits::{Read, Write};

    use crate::bundle::cbin::{CBinRead, CBinWrite};
    use crate::bundle::{BundleRead, BundleWrite};
    use crate::utils::slicebuf::SliceBuf;

    #[test]
    fn test_cbin_read() {
        fn test_contents(file: &[u8], expected: &[(&str, &[u8])]) {
            let mut buf = [0u8; 255];
            let mut reader = CBinRead::new(file, &mut buf);

            embassy_futures::block_on(async move {
                for (name, content) in expected {
                    let (n, blob) = reader.next_item().await.unwrap().unwrap();
                    assert_eq!(n, *name);

                    let mut data = [0; 255];
                    let data = read_all(blob, &mut data).await;

                    assert_eq!(data, *content);
                }

                assert!(reader.next_item().await.unwrap().is_none());
            });
        }

        async fn read_all<R: Read>(mut read: R, buf: &mut [u8]) -> &[u8] {
            let mut offset = 0;

            loop {
                if offset == buf.len() {
                    panic!("Buffer too small");
                }

                let n = read.read(&mut buf[offset..]).await.unwrap();
                if n == 0 {
                    break &buf[..offset];
                }

                offset += n;
            }
        }

        test_contents(b"", &[]);

        test_contents(b"\x03boo\xf4", &[("boo", b"")]);

        test_contents(
            b"\x04testdata\x01\xf4\xf4\xbc\xf4\xf4\xef\xf4",
            &[("test", b"data\x01\xf4\xbc\xf4\xef")],
        );

        test_contents(
            b"\x03foo\x01\x02\x03\xf4\x03bar\x04\x05\xf4",
            &[("foo", b"\x01\x02\x03"), ("bar", b"\x04\x05")],
        );
    }

    #[test]
    fn test_cbin_write() {
        fn test_contents(file: &[u8], items: &[(&str, &[u8])]) {
            let mut buf = [0u8; 1024];

            embassy_futures::block_on(async move {
                let mut buf = SliceBuf::new(&mut buf);

                {
                    let mut writer = CBinWrite::new(&mut buf);

                    for (name, content) in items {
                        let mut blob = writer.next_item(name).await.unwrap();
                        blob.write_all(content).await.unwrap();
                    }

                    writer.close().await.unwrap();
                }

                assert_eq!(buf.as_slice(), file);
            });
        }

        test_contents(b"", &[]);
        test_contents(b"\x03boo\xf4", &[("boo", b"")]);
        test_contents(
            b"\x04testdata\x01\xf4\xf4\xbc\xf4\xf4\xef\xf4",
            &[("test", b"data\x01\xf4\xbc\xf4\xef")],
        );
        test_contents(
            b"\x03foo\x01\x02\x03\xf4\x03bar\x04\x05\xf4",
            &[("foo", b"\x01\x02\x03"), ("bar", b"\x04\x05")],
        );
    }
}
