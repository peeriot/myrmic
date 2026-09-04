//! TLS 1.3 link: wraps a byte-stream transport in mTLS and exposes
//! [`LinkReceive`] / [`LinkSend`] halves compatible with the Zenoh-nano
//! transport layer.
//!
//! # Architecture
//!
//! ```text
//! TlsLinkRunner  (owns TlsConnection, pumps data)
//!      ↕                    ↕
//!  incoming channel    outgoing channel   (ZSlice, length-prefixed)
//!      ↓                    ↑
//! TlsLinkReceive       TlsLinkSend
//! (impl LinkReceive)  (impl LinkSend)
//!      ↓                    ↑
//!   Transport::accept / connect
//! ```
//!
//! The runner task handles framing with the same 2-byte LE length prefix used
//! by [`StreamingLinkReceive`] / [`StreamingLinkSend`], so the Zenoh transport
//! layer is unaffected.
//!
//! # Usage (WiFi/TCP path)
//!
//! 1. Establish TCP socket connection
//! 2. Build shared mTLS config: `MutualTlsConfig { ... }`
//! 3. Connect + split: `mtls_connect_and_split(socket, tls_buffers, mtls, channels, provider)`
//! 4. Spawn runner task
//! 5. Pass `TlsLinkReceive`/`TlsLinkSend` to `Network::connect`

extern crate alloc;

use embassy_sync::blocking_mutex::raw::{NoopRawMutex, RawMutex};
use embassy_sync::channel::{Receiver, Sender};
use zenoh_buffers::ZSlice;

use crate::link::{LinkError, LinkReceive, LinkSend};

/// MTU reported by TLS link halves to the Transport layer.
///
/// Sized for TLS `MaxFragmentLength::Bits9` (512-byte records) minus the 2-byte
/// `StreamingLink` length header.
pub const TLS_LINK_MTU: u16 = 510;

/// The receive half of a TLS link.  Implements [`LinkReceive`].
pub struct TlsLinkReceive<'a, M: RawMutex = NoopRawMutex> {
    pub(crate) receiver: Receiver<'a, M, ZSlice, 1>,
}

impl<M: RawMutex> LinkReceive for TlsLinkReceive<'_, M> {
    fn mtu(&self) -> u16 {
        TLS_LINK_MTU
    }

    async fn receive(&mut self) -> Result<ZSlice, LinkError> {
        Ok(self.receiver.receive().await)
    }
}

/// The send half of a TLS link.  Implements [`LinkSend`].
pub struct TlsLinkSend<'a, M: RawMutex = NoopRawMutex> {
    pub(crate) sender: Sender<'a, M, ZSlice, 1>,
}

impl<M: RawMutex> LinkSend for TlsLinkSend<'_, M> {
    fn mtu(&self) -> u16 {
        TLS_LINK_MTU
    }

    async fn send(&mut self, payload: ZSlice) -> Result<(), LinkError> {
        self.sender.send(payload).await;
        Ok(())
    }
}

#[cfg(feature = "tls")]
mod tls_impl {
    use alloc::vec;

    use embassy_futures::select::{Either, select};
    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use embedded_io_async::{Read, Write};
    use embedded_tls::{
        Aes128GcmSha256, Certificate, CryptoProvider, MaxFragmentLength, TlsConfig, TlsConnection,
        TlsContext, TlsError,
    };
    use zenoh_buffers::ZSlice;

    use super::{TLS_LINK_MTU, TlsLinkReceive, TlsLinkSend};

    /// Static read/write buffer size for a TLS connection.
    ///
    /// Sized for `MaxFragmentLength::Bits9` (512 B plaintext) plus the
    /// `TLS_RECORD_OVERHEAD` (128 B) constant from `embedded-tls`.
    pub const TLS_BUF_SIZE: usize = 1536;

    /// Static buffer storage for one TLS connection (allocate in `mk_static!`).
    pub struct TlsBuffers {
        /// TLS record receive buffer.
        pub rx: [u8; TLS_BUF_SIZE],
        /// TLS record transmit buffer.
        pub tx: [u8; TLS_BUF_SIZE],
    }

    impl TlsBuffers {
        /// Create zeroed TLS buffers.
        pub const fn new() -> Self {
            Self {
                rx: [0u8; TLS_BUF_SIZE],
                tx: [0u8; TLS_BUF_SIZE],
            }
        }
    }

    impl Default for TlsBuffers {
        fn default() -> Self {
            Self::new()
        }
    }

    /// Transport-neutral mTLS settings.
    ///
    /// All certificate/key values are DER-encoded bytes.
    #[derive(Clone, Copy)]
    pub struct MutualTlsConfig<'a> {
        /// Root CA used to validate the remote certificate chain.
        pub ca_certificate_der: &'a [u8],
        /// Local certificate presented during mTLS authentication.
        pub certificate_der: &'a [u8],
        /// Local private key (SEC1 DER for `embedded-tls`).
        pub private_key_der: &'a [u8],
        /// Optional server name for endpoint identity verification.
        pub server_name: Option<&'a str>,
        /// TLS max fragment length setting.
        pub max_fragment_length: MaxFragmentLength,
    }

    impl<'a> MutualTlsConfig<'a> {
        /// Build an [`embedded_tls::TlsConfig`] from this mTLS definition.
        pub fn to_tls_config(self) -> TlsConfig<'a> {
            let mut config = TlsConfig::new()
                .with_ca(Certificate::X509(self.ca_certificate_der))
                .with_cert(Certificate::X509(self.certificate_der))
                .with_priv_key(self.private_key_der)
                .with_max_fragment_length(self.max_fragment_length);

            if let Some(server_name) = self.server_name {
                config = config.with_server_name(server_name);
            }

            config
        }
    }

    /// Perform a TLS handshake over `socket` using the provided configuration
    /// and crypto provider.  Returns the established [`TlsConnection`] on success.
    pub async fn tls_connect<'io, 'cfg, S, P>(
        socket: S,
        rx_buf: &'io mut [u8],
        tx_buf: &'io mut [u8],
        config: &'cfg TlsConfig<'cfg>,
        provider: P,
    ) -> Result<TlsConnection<'io, S, Aes128GcmSha256>, TlsError>
    where
        S: Read + Write + 'io,
        P: CryptoProvider<CipherSuite = Aes128GcmSha256>,
    {
        let mut conn = TlsConnection::new(socket, rx_buf, tx_buf);
        conn.open(TlsContext::new(config, provider)).await?;
        Ok(conn)
    }

    /// Drives a [`TlsConnection`] and bridges it to channel-based link halves.
    ///
    /// Spawn this as an embassy task after calling [`tls_link_split`].
    ///
    /// # Framing
    ///
    /// Uses the same 2-byte LE length prefix as [`StreamingLinkReceive`] /
    /// [`StreamingLinkSend`].
    pub struct TlsLinkRunner<'a, S>
    where
        S: Read + Write + 'a,
    {
        tls: TlsConnection<'a, S, Aes128GcmSha256>,
        incoming_tx: embassy_sync::channel::Sender<'a, NoopRawMutex, ZSlice, 1>,
        outgoing_rx: embassy_sync::channel::Receiver<'a, NoopRawMutex, ZSlice, 1>,
        /// Partial length-header bytes received so far (0, 1, or 2).
        ///
        /// Preserved across select iterations so a cancelled read() does not
        /// lose a partially-received header byte.
        len_partial: [u8; 2],
        len_partial_n: usize,
    }

    impl<'a, S> TlsLinkRunner<'a, S>
    where
        S: Read + Write + 'a,
    {
        async fn send_frame(&mut self, payload: ZSlice) -> Result<(), TlsError> {
            let len_bytes = (payload.len() as u16).to_le_bytes();
            self.tls
                .write_all(&len_bytes)
                .await
                .map_err(|_| TlsError::IoError)?;
            self.tls
                .write_all(payload.as_ref())
                .await
                .map_err(|_| TlsError::IoError)?;
            self.tls.flush().await
        }

        /// Drive the TLS link until the connection closes or an error occurs.
        ///
        /// # Why a cooperative select loop instead of two independent tasks
        ///
        /// The natural design would be a read task and a write task running
        /// concurrently, each owning its half of the connection.  That requires
        /// splitting `TlsConnection`, which embassy-net achieves by routing all
        /// socket access through interior mutability (`with_mut` / `RefCell`).
        /// `TlsConnection` takes `&mut self` for both `read` and `write`, so the
        /// same trick is not available without wrapping it in a `Mutex`.
        /// `TlsConnection::split()` exists but requires `Socket: Clone`; the
        /// `TcpSocket` type used here does not implement `Clone`.
        ///
        /// The cooperative loop is therefore the correct pattern: one `&mut tls`
        /// at a time, interleaved via `select` between cancel-safe single-`read()`
        /// calls and send frames.  A single `read()` call is cancel-safe because
        /// it only returns bytes from the already-decrypted TLS record buffer —
        /// if the send branch fires and the read future is dropped, no bytes are
        /// consumed.  `len_partial_n` and `body_n` preserve accumulation progress
        /// across iterations.
        pub async fn run(mut self) -> Result<(), TlsError> {
            let mut msg_buf = vec![0u8; TLS_LINK_MTU as usize];
            loop {
                // Accumulate the 2-byte LE length header one read() at a time,
                // interleaving any pending outbound frames between header bytes.
                while self.len_partial_n < 2 {
                    let rest = &mut self.len_partial[self.len_partial_n..];
                    match select(self.outgoing_rx.receive(), self.tls.read(rest)).await {
                        Either::First(payload) => self.send_frame(payload).await?,
                        Either::Second(Ok(n)) if n > 0 => self.len_partial_n += n,
                        Either::Second(Ok(_)) => return Err(TlsError::IoError), // EOF
                        Either::Second(Err(_)) => return Err(TlsError::IoError),
                    }
                }

                let msg_len = u16::from_le_bytes(self.len_partial) as usize;
                self.len_partial_n = 0;

                if msg_len == 0 || msg_len > TLS_LINK_MTU as usize {
                    return Err(TlsError::InvalidRecord);
                }

                debug_assert!(msg_len <= msg_buf.len());
                // Accumulate the message body one read() at a time, interleaving
                // outbound frames so the send channel never stalls behind a
                // multi-record body read.
                let mut body_n = 0;
                while body_n < msg_len {
                    let rest = &mut msg_buf[body_n..msg_len];
                    match select(self.outgoing_rx.receive(), self.tls.read(rest)).await {
                        Either::First(payload) => self.send_frame(payload).await?,
                        Either::Second(Ok(n)) if n > 0 => body_n += n,
                        Either::Second(Ok(_)) => return Err(TlsError::IoError), // EOF
                        Either::Second(Err(_)) => return Err(TlsError::IoError),
                    }
                }

                self.incoming_tx
                    .send(msg_buf[..msg_len].to_vec().into())
                    .await;
            }
        }
    }

    /// Static channel storage for a [`TlsLink`].
    pub struct TlsLinkChannels {
        incoming: embassy_sync::channel::Channel<NoopRawMutex, ZSlice, 1>,
        outgoing: embassy_sync::channel::Channel<NoopRawMutex, ZSlice, 1>,
    }

    impl TlsLinkChannels {
        /// Create new zeroed channel storage.
        pub const fn new() -> Self {
            Self {
                incoming: embassy_sync::channel::Channel::new(),
                outgoing: embassy_sync::channel::Channel::new(),
            }
        }
    }

    impl Default for TlsLinkChannels {
        fn default() -> Self {
            Self::new()
        }
    }

    /// Establish an mTLS client connection and split it into link halves.
    ///
    /// Transport-neutral: works with any stream implementing
    /// [`embedded_io_async::Read`] + [`embedded_io_async::Write`].
    pub async fn mtls_connect_and_split<'io, S, P>(
        socket: S,
        tls_buffers: &'io mut TlsBuffers,
        mtls: MutualTlsConfig<'io>,
        channels: &'io TlsLinkChannels,
        provider: P,
    ) -> Result<(TlsLinkRunner<'io, S>, TlsLinkReceive<'io>, TlsLinkSend<'io>), TlsError>
    where
        S: Read + Write + 'io,
        P: CryptoProvider<CipherSuite = Aes128GcmSha256>,
    {
        let config = mtls.to_tls_config();
        let tls = tls_connect(
            socket,
            &mut tls_buffers.rx,
            &mut tls_buffers.tx,
            &config,
            provider,
        )
        .await?;
        Ok(tls_link_split(tls, channels))
    }

    /// Split an established [`TlsConnection`] into a runner and channel-based
    /// link halves.
    ///
    /// # Returns
    ///
    /// `(runner, receive, send)`
    ///
    /// - `runner` — must be spawned as an embassy task
    /// - `receive` — implements [`LinkReceive`]; pass to `Network::connect`
    /// - `send`    — implements [`LinkSend`];    pass to `Network::connect`
    pub fn tls_link_split<'a, S>(
        tls: TlsConnection<'a, S, Aes128GcmSha256>,
        channels: &'a TlsLinkChannels,
    ) -> (TlsLinkRunner<'a, S>, TlsLinkReceive<'a>, TlsLinkSend<'a>)
    where
        S: Read + Write + 'a,
    {
        let runner = TlsLinkRunner {
            tls,
            incoming_tx: channels.incoming.sender(),
            outgoing_rx: channels.outgoing.receiver(),
            len_partial: [0u8; 2],
            len_partial_n: 0,
        };
        let receive = TlsLinkReceive {
            receiver: channels.incoming.receiver(),
        };
        let send = TlsLinkSend {
            sender: channels.outgoing.sender(),
        };
        (runner, receive, send)
    }
}

#[cfg(feature = "tls")]
pub use tls_impl::*;
