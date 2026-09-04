//! L2CAP CoC byte-stream adapter for `embedded-tls`.

use bt_hci::controller::Controller;
use embassy_time::{Duration, with_timeout};
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use trouble_host::l2cap::{L2capChannel, L2capChannelConfig};
use trouble_host::prelude::DefaultPacketPool;
use trouble_host::{PacketPool, Stack};

/// PSM (Protocol/Service Multiplexer) for Swarm's mTLS L2CAP channels.
pub const SWARM_TLS_PSM: u16 = 0x00F0;

/// Maximum size of one L2CAP SDU used by the TLS channel.
pub const SDU_MTU: usize = 247;

/// Timeout for a single L2CAP send/receive operation.
const L2CAP_IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Recommended L2CAP channel configuration for Swarm's mTLS channel.
pub fn swarm_l2cap_config() -> L2capChannelConfig {
    L2capChannelConfig {
        mtu: Some(SDU_MTU as u16),
        ..Default::default()
    }
}

/// Byte-stream adapter over a BLE L2CAP CoC channel.
pub struct L2capStream<'d, 's, C, P = DefaultPacketPool>
where
    C: Controller,
    P: PacketPool,
{
    channel: L2capChannel<'d, P>,
    stack: &'s Stack<'s, C, P>,
    /// Internal buffer holding one SDU's worth of bytes.
    read_buf: [u8; SDU_MTU],
    /// Index of the next byte to hand to the caller.
    read_pos: usize,
    /// Number of valid bytes in `read_buf` from the most recent SDU.
    read_len: usize,
}

impl<'d, 's, C, P> L2capStream<'d, 's, C, P>
where
    C: Controller,
    P: PacketPool,
{
    /// Create a new adapter from an already-established L2CAP CoC channel.
    pub fn new(channel: L2capChannel<'d, P>, stack: &'s Stack<'s, C, P>) -> Self {
        Self {
            channel,
            stack,
            read_buf: [0u8; SDU_MTU],
            read_pos: 0,
            read_len: 0,
        }
    }

    /// Consume the adapter and return the inner [`L2capChannel`].
    pub fn into_channel(self) -> L2capChannel<'d, P> {
        self.channel
    }
}

/// Error type for [`L2capStream`].
#[derive(Debug, Copy, Clone)]
pub struct L2capStreamError(ErrorKind);

impl embedded_io_async::Error for L2capStreamError {
    fn kind(&self) -> ErrorKind {
        self.0
    }
}

impl core::fmt::Display for L2capStreamError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "L2CAP I/O error: {:?}", self.0)
    }
}

impl core::error::Error for L2capStreamError {}

impl<'d, 's, C, P> ErrorType for L2capStream<'d, 's, C, P>
where
    C: Controller,
    P: PacketPool,
{
    type Error = L2capStreamError;
}

impl<'d, 's, C, P> Read for L2capStream<'d, 's, C, P>
where
    C: Controller,
    P: PacketPool,
{
    /// Read bytes from the L2CAP channel.
    ///
    /// Returns data from the internal buffer if bytes remain from a prior SDU;
    /// otherwise waits for the next SDU and returns bytes from it.
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Ok(0);
        }

        if self.read_pos >= self.read_len {
            let n = with_timeout(
                L2CAP_IO_TIMEOUT,
                self.channel.receive(self.stack, &mut self.read_buf),
            )
            .await
            .map_err(|_| {
                error!("[L2CAP] receive timeout after {:?}", L2CAP_IO_TIMEOUT);
                L2capStreamError(ErrorKind::TimedOut)
            })?
            .map_err(|e| {
                error!("[L2CAP] receive failed: {:?}", e);
                L2capStreamError(ErrorKind::Other)
            })?;
            if n == 0 {
                return Err(L2capStreamError(ErrorKind::ConnectionReset));
            }
            self.read_pos = 0;
            self.read_len = n;
        }

        let available = self.read_len - self.read_pos;
        let to_copy = available.min(buf.len());
        buf[..to_copy].copy_from_slice(&self.read_buf[self.read_pos..self.read_pos + to_copy]);
        self.read_pos += to_copy;

        Ok(to_copy)
    }
}

impl<'d, 's, C, P> Write for L2capStream<'d, 's, C, P>
where
    C: Controller,
    P: PacketPool,
{
    /// Write bytes to the L2CAP channel as a single SDU.
    ///
    /// Sends at most [`SDU_MTU`] bytes per call; `write_all` (provided by the
    /// trait) will loop until the full buffer is consumed.
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        let to_send = buf.len().min(SDU_MTU);
        with_timeout(
            L2CAP_IO_TIMEOUT,
            self.channel.send(self.stack, &buf[..to_send]),
        )
        .await
        .map_err(|_| {
            error!(
                "[L2CAP] send timeout after {:?} ({} bytes)",
                L2CAP_IO_TIMEOUT, to_send
            );
            L2capStreamError(ErrorKind::TimedOut)
        })?
        .map_err(|e| {
            error!("[L2CAP] send failed: {:?}", e);
            L2capStreamError(ErrorKind::Other)
        })?;
        Ok(to_send)
    }

    /// L2CAP sends are committed immediately; flush is a no-op.
    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}
