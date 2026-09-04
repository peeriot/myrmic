// Derived from `storekey` 0.6.0, src/decode/read.rs. Modified by Peeriot GmbH.
// Upstream carries no copyright notice in this file; see NOTICE for
// attribution and LICENSES/Apache-2.0.txt for the licence.

use std::io::{self, BufRead, ErrorKind, Read};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Reference<'b, 'c> {
    /// Borrowed from the buffer.
    Borrowed(&'b [u8]),
    /// Copied from the input onto the heap.
    Copied(&'c [u8]),
}

/// For zero-copy reading.
pub trait ReadReference<'de>: Read + BufRead {
    /// Reads an exact number of bytes.
    fn read_reference<'a>(&'a mut self, len: usize) -> Result<Reference<'de, 'a>, io::Error>;

    /// Reads bytes until a delimiter, excluding the delimiter.
    fn read_reference_until<'a>(
        &'a mut self,
        delimiter: u8,
    ) -> Result<Reference<'de, 'a>, io::Error>;
}

#[derive(Debug)]
pub struct SliceReader<'a> {
    inner: &'a [u8],
}

impl<'a> SliceReader<'a> {
    #[inline]
    pub fn new(inner: &'a [u8]) -> Self {
        Self { inner }
    }
}

impl Read for SliceReader<'_> {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, io::Error> {
        self.inner.read(buf)
    }

    #[inline]
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), io::Error> {
        self.inner.read_exact(buf)
    }
}

impl BufRead for SliceReader<'_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.inner.fill_buf()
    }

    fn consume(&mut self, amt: usize) {
        self.inner.consume(amt);
    }
}

impl<'de> ReadReference<'de> for SliceReader<'de> {
    fn read_reference<'a>(&'a mut self, len: usize) -> Result<Reference<'de, 'a>, io::Error> {
        if len > self.inner.len() {
            return Err(ErrorKind::UnexpectedEof.into());
        }
        let (a, b) = self.inner.split_at(len);
        self.inner = b;
        Ok(Reference::Borrowed(a))
    }

    #[inline]
    fn read_reference_until<'a>(
        &'a mut self,
        delimiter: u8,
    ) -> Result<Reference<'de, 'a>, io::Error> {
        if let Some(end) = memchr::memchr(delimiter, self.inner) {
            let (before, after) = self.inner.split_at(end);
            self.inner = &after[1..];
            Ok(Reference::Borrowed(before))
        } else {
            Err(io::Error::new(ErrorKind::UnexpectedEof, "unexpected EOF"))
        }
    }
}
