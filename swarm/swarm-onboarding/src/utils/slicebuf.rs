//! Utilities for working with byte slices.

use core::ops::Deref;

use crate::utils::BufferOverflowError;

/// A buffer backed by a mutable byte slice that can be extended, implements the `Write` trait and can be formatted into.
pub struct SliceBuf<'a> {
    /// The underlying buffer.
    buf: &'a mut [u8],
    /// The current offset in the buffer.
    offset: usize,
}

impl<'a> SliceBuf<'a> {
    /// Create a new `SliceBuf` with the given buffer.
    ///
    /// # Arguments
    /// - `buf`: The buffer to use.
    ///
    /// # Returns
    /// - A new `SliceBuf`.
    pub const fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, offset: 0 }
    }

    /// Get the current contents of the buffer as a slice.
    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..self.offset]
    }

    /// Split the buffer into the current contents and the remaining space.
    ///
    /// # Returns
    /// - A tuple containing the current contents and the remaining space.
    pub fn split(self) -> (&'a mut [u8], &'a mut [u8]) {
        let (slice, buf) = self.buf.split_at_mut(self.offset);

        (slice, buf)
    }

    /// Split the buffer into the current contents as a UTF-8 string and the remaining space.
    ///
    /// # Returns
    /// - `Ok((str, buf))`: The current contents as a UTF-8 string and the remaining space.
    /// - `Err(core::str::Utf8Error)`: The current contents are not valid UTF-8.
    pub fn split_str(self) -> Result<(&'a str, &'a mut [u8]), core::str::Utf8Error> {
        let (slice, buf) = self.split();

        Ok((core::str::from_utf8(slice)?, buf))
    }

    /// Extend the buffer with the given data.
    ///
    /// # Arguments
    /// - `data`: An iterator over the data to extend the buffer with.
    ///
    /// # Returns
    /// - `Ok(())`: The buffer was successfully extended.
    /// - `Err(BufferOverflowError)`: The buffer overflowed.
    pub fn extend<I: Iterator<Item = u8>>(&mut self, data: I) -> Result<(), BufferOverflowError> {
        let mut offset = self.offset;

        for byte in data {
            if offset == self.buf.len() {
                return Err(BufferOverflowError);
            }

            self.buf[offset] = byte;
            offset += 1;
        }

        self.offset = offset;

        Ok(())
    }
}

impl AsRef<[u8]> for SliceBuf<'_> {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl Deref for SliceBuf<'_> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl ErrorType for SliceBuf<'_> {
    type Error = BufferOverflowError;
}

impl Write for SliceBuf<'_> {
    async fn write(&mut self, data: &[u8]) -> Result<usize, Self::Error> {
        if data.is_empty() {
            return Ok(0);
        }

        let to_copy = core::cmp::min(data.len(), self.buf.len() - self.offset);
        if to_copy == 0 {
            return Err(BufferOverflowError);
        }

        self.buf[self.offset..self.offset + to_copy].copy_from_slice(&data[..to_copy]);
        self.offset += to_copy;

        Ok(to_copy)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl core::fmt::Write for SliceBuf<'_> {
    fn write_str(&mut self, str: &str) -> core::fmt::Result {
        self.extend(str.as_bytes().iter().copied())
            .map_err(|_| core::fmt::Error)
    }
}

/// A macro to write formatted data into a byte slice using a `SliceBuf`.
macro_rules! write_buf {
    ($dst:expr, $($arg:tt)*) => {{
        use core::fmt::Write as _;

        let mut buf = $crate::utils::slicebuf::SliceBuf::new($dst);

        buf.write_fmt(format_args!($($arg)*))?;

        Result::<_, core::fmt::Error>::Ok(buf.split_str().unwrap())
    }};
}

pub(crate) use write_buf;
use zenoh_traits::{ErrorType, Write};
