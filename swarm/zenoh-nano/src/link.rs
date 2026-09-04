//! Link
//!
//! Provides traits for sending and receiving binary message payloads
//! over various transports (like TCP, UDP, serial, BLE, etc.),
//! thus abstracting this layer from the rest of the Zenoh stack.
//!
//! The layering of the Zenoh networking stack is as follows:
//! Network (send/receive `NetworkMessage` instances)
//!    -> Transport (send/receive `TransportMessage` instances)
//!       -> Link (send/receive binary payloads)
#![allow(async_fn_in_trait)]

use core::future::Future;

use alloc::vec;

use embassy_sync::blocking_mutex::raw::{NoopRawMutex, RawMutex};

use embedded_io_async::{Error, ErrorKind, ErrorType, Read, ReadExactError, Write};

use zenoh_buffers::ZSlice;

#[cfg(feature = "trouble-host")]
pub mod trouble;

/// BLE L2CAP CoC byte-stream adapter for use as a TLS transport.
#[cfg(feature = "trouble-host")]
pub mod l2cap;

/// TLS 1.3 link runner and channel-based [`LinkReceive`] / [`LinkSend`] halves.
#[cfg(feature = "tls")]
pub mod tls;

/// Link error
#[derive(thiserror::Error, Debug, Copy, Clone, Eq, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum LinkError {
    /// The payload is too large for the link's MTU
    #[error("payload too large")]
    PayloadTooLarge,
    /// An unexpected end of stream was encountered while reading
    #[error("unexpected end of stream")]
    UnexpectedEof,
    /// An I/O error occurred
    #[error("I/O error: {0}")]
    Io(#[from] ErrorKind),
}

impl<E> From<ReadExactError<E>> for LinkError
where
    E: Error,
{
    fn from(e: ReadExactError<E>) -> Self {
        match e {
            ReadExactError::UnexpectedEof => LinkError::UnexpectedEof,
            ReadExactError::Other(e) => LinkError::Io(e.kind()),
        }
    }
}

/// The receive half of a link
pub trait LinkReceive {
    /// The Maximum Transmission Unit that the link supports
    fn mtu(&self) -> u16;

    /// The size of any headers included in the MTU
    fn mtu_header_size(&self) -> u16 {
        0
    }

    /// Receive a binary message payload from the link
    async fn receive(&mut self) -> Result<ZSlice, LinkError>;
}

impl<T> LinkReceive for &mut T
where
    T: LinkReceive,
{
    fn mtu(&self) -> u16 {
        (**self).mtu()
    }

    fn mtu_header_size(&self) -> u16 {
        (**self).mtu_header_size()
    }

    fn receive(&mut self) -> impl Future<Output = Result<ZSlice, LinkError>> {
        (*self).receive()
    }
}

/// The send half of a link
pub trait LinkSend {
    /// The Maximum Transmission Unit that the link supports
    fn mtu(&self) -> u16;

    /// The size of any headers included in the MTU
    fn mtu_header_size(&self) -> u16 {
        0
    }

    /// Send a binary message payload over the link
    async fn send(&mut self, payload: ZSlice) -> Result<(), LinkError>;
}

impl<T> LinkSend for &mut T
where
    T: LinkSend,
{
    fn mtu(&self) -> u16 {
        (**self).mtu()
    }

    fn mtu_header_size(&self) -> u16 {
        (**self).mtu_header_size()
    }

    fn send(&mut self, payload: ZSlice) -> impl Future<Output = Result<(), LinkError>> {
        (*self).send(payload)
    }
}

/// An implementation of `LinkReceive` suitable for streaming protocols
/// like pipes and TCP/TLS sockets.
///
/// The payload is prefixed with a 2-byte little-endian length field.
///
/// The MTU specified when creating the link is the maximum payload size,
/// not including the 2-byte length field.
///
/// NOTE: The future returned by this `receive` implementation is NOT cancellation safe!
/// In other words, if the future is dropped before it completes, the underlying stream
/// may be left in an inconsistent state in that only half of a message might be read (and lost).
pub struct StreamingLinkReceive<R>(R, u16);

impl<R> StreamingLinkReceive<R> {
    /// Create a new `StreamingLinkReceive` wrapping the given reader and MTU.
    ///
    /// # Arguments
    /// - `read`: The reader to wrap
    /// - `mtu`: The maximum payload size (including the 2-byte length field)
    #[must_use]
    pub const fn new(read: R, mtu: u16) -> Self {
        Self(read, mtu)
    }
}

impl<R: Read> LinkReceive for StreamingLinkReceive<R> {
    fn mtu(&self) -> u16 {
        self.1
    }

    fn mtu_header_size(&self) -> u16 {
        2
    }

    async fn receive(&mut self) -> Result<ZSlice, LinkError> {
        let mut len_buf = [0u8; 2];
        self.0.read_exact(&mut len_buf).await?;

        let len = u16::from_le_bytes(len_buf);
        if len > self.mtu() {
            return Err(LinkError::PayloadTooLarge);
        }

        let mut buf = vec![0u8; len as usize];
        self.0.read_exact(&mut buf).await?;

        Ok(ZSlice::from(buf))
    }
}

/// An implementation of `LinkSend` suitable for streaming protocols
/// like pipes and TCP/TLS sockets.
///
/// The payload is prefixed with a 2-byte little-endian length field.
///
/// The MTU specified when creating the link is the maximum payload size,
/// not including the 2-byte length field.
///
/// NOTE: The future returned by this `send` implementation is NOT cancellation safe!
/// In other words, if the future is dropped before it completes, the underlying stream
/// may be left in an inconsistent state in that only half of a message might be sent, and the rest might be lost.
pub struct StreamingLinkSend<W>(W, u16);

impl<W> StreamingLinkSend<W> {
    /// Create a new `StreamingLinkSend` wrapping the given writer and MTU.
    ///
    /// # Arguments
    /// - `write`: The writer to wrap
    /// - `mtu`: The maximum payload size (including the 2-byte length field)
    #[must_use]
    pub const fn new(write: W, mtu: u16) -> Self {
        Self(write, mtu)
    }
}

impl<W: embedded_io_async::Write> LinkSend for StreamingLinkSend<W> {
    fn mtu(&self) -> u16 {
        self.1
    }

    fn mtu_header_size(&self) -> u16 {
        2
    }

    async fn send(&mut self, payload: ZSlice) -> Result<(), LinkError> {
        let len = payload.len();
        if len > self.mtu() as usize {
            return Err(LinkError::PayloadTooLarge);
        }

        let len_buf = (len as u16).to_le_bytes();
        self.0
            .write_all(&len_buf)
            .await
            .map_err(|e| LinkError::Io(e.kind()))?;

        self.0
            .write_all(payload.as_ref())
            .await
            .map_err(|e| LinkError::Io(e.kind()))?;

        self.0.flush().await.map_err(|e| LinkError::Io(e.kind()))?;

        Ok(())
    }
}

const PIPE_LINK_BUF_SIZE: usize = 200;

/// A link implementation using an in-memory pipe as the underlying transport.
///
/// Useful for unit and integration testing when two sessions need to be connected together
/// in a single program.
pub type PipeLink<M = NoopRawMutex> = embassy_sync::pipe::Pipe<M, PIPE_LINK_BUF_SIZE>;

/// The receive half of an in-memory pipe link.
pub type PipeLinkReceive<'a, M = NoopRawMutex> = StreamingLinkReceive<PipeRead<'a, M>>;

/// The send half of an in-memory pipe link.
pub type PipeLinkSend<'a, M = NoopRawMutex> = StreamingLinkSend<PipeWrite<'a, M>>;

/// Adapts the read half of the pipe to embedded-io-async Read V0.7
///
/// Necessary because embassy-sync V0.7 is still based on embedded-io-async V0.6
pub struct PipeRead<'a, M: RawMutex = NoopRawMutex>(
    embassy_sync::pipe::Reader<'a, M, PIPE_LINK_BUF_SIZE>,
);

impl<'a, M: RawMutex> PipeRead<'a, M> {
    /// Construct a new PipeRead
    pub fn new(reader: embassy_sync::pipe::Reader<'a, M, PIPE_LINK_BUF_SIZE>) -> Self {
        Self(reader)
    }
}

impl<M: RawMutex> ErrorType for PipeRead<'_, M> {
    type Error = core::convert::Infallible;
}

impl<M: RawMutex> Read for PipeRead<'_, M> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        Ok(self.0.read(buf).await)
    }
}

/// Adapts the write half of the pipe to embedded-io-async Write V0.7
///
/// Necessary because embassy-sync V0.7 is still based on embedded-io-async V0.6
pub struct PipeWrite<'a, M: RawMutex = NoopRawMutex>(
    embassy_sync::pipe::Writer<'a, M, PIPE_LINK_BUF_SIZE>,
);

impl<'a, M: RawMutex> PipeWrite<'a, M> {
    /// Construct a new PipeWrite
    pub fn new(writer: embassy_sync::pipe::Writer<'a, M, PIPE_LINK_BUF_SIZE>) -> Self {
        Self(writer)
    }
}

impl<M: RawMutex> ErrorType for PipeWrite<'_, M> {
    type Error = core::convert::Infallible;
}

impl<M: RawMutex> Write for PipeWrite<'_, M> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        Ok(self.0.write(buf).await)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}
