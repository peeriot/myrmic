/// This module contains IO wrappers to add AES-GCM encryption/decryption
use aes_gcm::aead::AeadMutInPlace;
use aes_gcm::{Aes256Gcm, Key, KeyInit as _};

use elliptic_curve::rand_core::RngCore;

use embedded_io_async::{Error, ErrorKind, ErrorType, Read, ReadExactError, Write};

use crate::io::{EitherWrapper, NoopWrapper, ReadWrapper, WriteWrapper};

/// Create a crypto-enabled reader wrapper if a key is provided.
///
/// # Arguments
/// - `key`: An optional AES-GCM key.
/// - `buf`: A mutable buffer to use for decryption.
///
/// # Returns
/// - `(wrapper, buf)`: A tuple containing either a `CryptoReadWrapper` or a `NoopWrap`, and the remaining buffer space.
pub fn create_crypto_read_opt<'a>(
    key: Option<&Key<Aes256Gcm>>,
    buf: &'a mut [u8],
) -> (
    EitherWrapper<CryptoReadWrapper<'a>, NoopWrapper>,
    &'a mut [u8],
) {
    if let Some(key) = key {
        let (crypto_buf, buf) = buf.split_at_mut(512);
        let crypto = Aes256Gcm::new(key);

        (
            EitherWrapper::First(CryptoReadWrapper::new(crypto, crypto_buf)),
            buf,
        )
    } else {
        (EitherWrapper::Second(NoopWrapper), buf)
    }
}

/// Create a crypto-enabled writer wrapper if a key is provided.
///
/// # Arguments
/// - `key`: An optional AES-GCM key.
/// - `rng`: A mutable reference to a random number generator.
/// - `buf`: A mutable buffer to use for encryption.
///
/// # Returns
/// - `(wrapper, buf)`: A tuple containing either a `CryptoWriteWrapper` or a `NoopWrap`, and the remaining buffer space.
pub fn create_crypto_write_opt<'a, C>(
    key: Option<&Key<Aes256Gcm>>,
    rng: &'a mut C,
    buf: &'a mut [u8],
) -> (
    EitherWrapper<CryptoWriteWrapper<'a, C>, NoopWrapper>,
    &'a mut [u8],
)
where
    C: RngCore,
{
    if let Some(key) = key {
        let (crypto_buf, buf) = buf.split_at_mut(512);
        let crypto = Aes256Gcm::new(key);

        (
            EitherWrapper::First(CryptoWriteWrapper::new(crypto, rng, crypto_buf)),
            buf,
        )
    } else {
        (EitherWrapper::Second(NoopWrapper), buf)
    }
}

/// A wrapper to create crypto-enabled readers.
pub struct CryptoReadWrapper<'a> {
    /// The cypher to use for decryption
    cypher: Aes256Gcm,
    /// The buffer to use for decryption
    buf: &'a mut [u8],
}

impl<'a> CryptoReadWrapper<'a> {
    /// Create a new `CryptoReadWrapper` with the given cypher and buffer.
    ///
    /// # Arguments
    /// - `cypher`: The cypher to use for decryption.
    /// - `buf`: The buffer to use for decryption.
    pub const fn new(cypher: Aes256Gcm, buf: &'a mut [u8]) -> Self {
        Self { cypher, buf }
    }
}

impl<'a> ReadWrapper for CryptoReadWrapper<'a> {
    type Reader<'t, R>
        = CryptoReader<'t, R>
    where
        Self: 't,
        R: Read;

    fn wrap<R: Read>(&mut self, read: R) -> Self::Reader<'_, R> {
        CryptoReader::new(&mut self.cypher, self.buf, read)
    }
}

/// A wrapper to create crypto-enabled writers.
pub struct CryptoWriteWrapper<'a, C> {
    /// The cypher to use for encryption
    cypher: Aes256Gcm,
    /// The RNG to use for nonce generation
    rng: &'a mut C,
    /// The buffer to use for encryption
    buf: &'a mut [u8],
}

impl<'a, C> CryptoWriteWrapper<'a, C> {
    /// Create a new `CryptoWriteWrapper` with the given cypher, RNG and buffer.
    ///
    /// # Arguments
    /// - `cypher`: The cypher to use for encryption.
    /// - `rng`: The RNG to use for nonce generation.
    /// - `buf`: The buffer to use for encryption.
    pub const fn new(cypher: Aes256Gcm, rng: &'a mut C, buf: &'a mut [u8]) -> Self {
        Self { cypher, rng, buf }
    }
}

impl<'a, C> WriteWrapper for CryptoWriteWrapper<'a, C>
where
    C: RngCore,
{
    type Writer<'t, W>
        = CryptWriter<'t, C, W>
    where
        Self: 't,
        W: Write;

    fn wrap<W: Write>(&mut self, write: W) -> Self::Writer<'_, W> {
        CryptWriter::new(&mut self.cypher, self.rng, self.buf, write)
    }
}

/// A reader that decrypts data read from an underlying reader on-the-fly
///
/// NOTE:
/// The `Read::read` implementation this type provides is NOT cancel-safe.
///
/// TODO: Generify to any AesGcm
pub struct CryptoReader<'a, R> {
    /// The cypher to use for decryption
    cypher: &'a mut Aes256Gcm,
    /// The buffer to use for decryption
    buf: &'a mut [u8],
    /// The current offset in the buffer pointing to data which is not retrieved yet
    /// via the `read` method
    offset: usize,
    /// The amount of data currently loaded in the buffer
    loaded: usize,
    /// The underlying reader to read encrypted data from.
    read: R,
}

impl<'a, R> CryptoReader<'a, R>
where
    R: Read,
{
    /// Create a new `CryptoReader` with the given key and underlying reader.
    ///
    /// # Arguments
    /// - `cypher`: The cypher to use for decryption.
    /// - `buf`: The buffer to use for decryption.
    /// - `read`: The underlying reader to read encrypted data from.
    pub const fn new(cypher: &'a mut Aes256Gcm, buf: &'a mut [u8], read: R) -> Self {
        Self {
            cypher,
            buf,
            offset: 0,
            loaded: 0,
            read,
        }
    }

    /// Load and decrypt the next message into the buffer.
    ///
    /// If there is already data in the buffer that has not been read yet, this method does nothing.
    async fn load(&mut self) -> Result<(), ErrorKind> {
        if self.offset < self.loaded {
            return Ok(());
        }

        self.offset = 0;
        self.loaded = 0;

        let mut nonce = [0u8; 12];
        let len = self
            .read
            .read(&mut nonce[..1])
            .await
            .map_err(|e| e.kind())?;
        if len == 0 {
            // Eof
            return Ok(());
        }

        self.read
            .read_exact(&mut nonce[1..])
            .await
            .map_err(Self::map_err)?;

        let mut tag = [0u8; 16];
        self.read
            .read_exact(&mut tag)
            .await
            .map_err(Self::map_err)?;

        let mut len_bytes = [0u8; 2];
        self.read
            .read_exact(&mut len_bytes)
            .await
            .map_err(Self::map_err)?;

        let len = u16::from_le_bytes(len_bytes) as usize;
        self.read
            .read_exact(&mut self.buf[..len])
            .await
            .map_err(Self::map_err)?;

        self.cypher
            .decrypt_in_place_detached(&nonce.into(), b"", &mut self.buf[..len], &tag.into())
            .map_err(|_| ErrorKind::InvalidData)?;

        self.offset = 0;
        self.loaded = len;

        Ok(())
    }

    fn map_err<E: Error>(e: ReadExactError<E>) -> ErrorKind {
        match e {
            ReadExactError::Other(e) => e.kind(),
            ReadExactError::UnexpectedEof => ErrorKind::InvalidData,
        }
    }
}

impl<R> ErrorType for CryptoReader<'_, R>
where
    R: ErrorType,
{
    type Error = ErrorKind;
}

impl<R> Read for CryptoReader<'_, R>
where
    R: Read,
{
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.load().await?;

        let len = buf.len().min(self.loaded - self.offset);

        buf[..len].copy_from_slice(&self.buf[self.offset..self.offset + len]);
        self.offset += len;

        Ok(len)
    }
}

/// A writer that encrypts data on-the-fly before writing it to an underlying writer.
///
/// NOTE:
/// The `Write::write` implementation this type provides is NOT cancel-safe.
///
/// TODO: Generify to any AesGcm
pub struct CryptWriter<'a, C, W> {
    /// The cypher to use for encryption.
    cypher: &'a mut Aes256Gcm,
    /// The RNG to use for nonce generation.
    rng: &'a mut C,
    /// The buffer to use for encryption.
    buf: &'a mut [u8],
    /// The amount of data currently filled in the buffer.
    filled: usize,
    /// The underlying writer to write the encrypted data to.
    write: W,
}

impl<'a, C, W> CryptWriter<'a, C, W>
where
    C: RngCore,
    W: Write,
{
    /// Create a new `CryptWriter` with the given key and underlying writer.
    ///
    /// # Arguments
    /// - `cypher`: The cypher to use for encryption.
    /// - `rng`: The RNG to use for nonce generation.
    /// - `buf`: The buffer to use for encryption.
    /// - `write`: The underlying writer to write the encrypted data to.
    pub const fn new(
        cypher: &'a mut Aes256Gcm,
        rng: &'a mut C,
        buf: &'a mut [u8],
        write: W,
    ) -> Self {
        Self {
            cypher,
            rng,
            buf,
            filled: 0,
            write,
        }
    }

    /// Send the currently filled buffer as an encrypted message.
    /// If the buffer is empty, this method does nothing.
    ///
    /// Returns `Ok(())` if the message was sent successfully, or an `ErrorKind` if an error occurred.
    async fn send(&mut self) -> Result<(), ErrorKind> {
        if self.filled == 0 {
            return Ok(());
        }

        let mut nonce = [0u8; 12];
        self.rng.fill_bytes(&mut nonce);

        let tag = unwrap!(self.cypher.encrypt_in_place_detached(
            &nonce.into(),
            b"",
            &mut self.buf[..self.filled]
        ));

        self.write.write_all(&nonce).await.map_err(Self::map_err)?;
        self.write
            .write_all(tag.as_ref())
            .await
            .map_err(Self::map_err)?;
        self.write
            .write_all(&(self.filled as u16).to_le_bytes())
            .await
            .map_err(Self::map_err)?;
        self.write
            .write_all(&self.buf[..self.filled])
            .await
            .map_err(Self::map_err)?;

        self.filled = 0;

        Ok(())
    }

    fn map_err<E: Error>(e: E) -> ErrorKind {
        e.kind()
    }
}

impl<C, W> ErrorType for CryptWriter<'_, C, W>
where
    W: ErrorType,
{
    type Error = ErrorKind;
}

impl<C, W> Write for CryptWriter<'_, C, W>
where
    C: RngCore,
    W: Write,
{
    async fn write(&mut self, data: &[u8]) -> Result<usize, Self::Error> {
        if self.filled == self.buf.len() {
            self.send().await?;
        }

        let len = data.len().min(self.buf.len() - self.filled);

        self.buf[self.filled..self.filled + len].copy_from_slice(&data[..len]);
        self.filled += len;

        Ok(len)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.send().await?;
        self.write.flush().await.map_err(Self::map_err)
    }
}
