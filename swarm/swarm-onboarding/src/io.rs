//! Traits and implementations for stream consumers and producers.

use embedded_io_async::{Error, ErrorKind, ErrorType, Read, Write};

use serde::{Deserialize, Serialize};

use crate::utils::io::read_all;

/// A trait representing a consumer of a stream.
pub trait StreamConsumer: ErrorType {
    /// Consume the provided stream.
    ///
    /// # Arguments
    /// - `stream`: The read stream of the bundle.
    /// - `progress_notify`: A closure that might be called by the consumer to notify progress in consuming the stream.
    ///
    /// # Returns
    /// - `Ok(())`: The bundle was consumed successfully.
    /// - `Err(e)`: An error occurred while consuming the bundle.
    async fn consume<R: Read, F: FnMut()>(
        &mut self,
        stream: R,
        progress_notify: F,
    ) -> Result<(), Self::Error>;
}

impl<T> StreamConsumer for &mut T
where
    T: StreamConsumer,
{
    async fn consume<R: Read, F: FnMut()>(
        &mut self,
        stream: R,
        progress_notify: F,
    ) -> Result<(), Self::Error> {
        (*self).consume(stream, progress_notify).await
    }
}

/// A trait representing a producer of a stream.
pub trait StreamProducer: ErrorType {
    /// Produce the stream.
    ///
    /// # Arguments
    /// - `stream`: A writer for the stream.
    /// - `progress_notify`: A closure that might be called by the producer to notify progress in producing the stream.
    ///
    /// # Returns
    /// - `Ok(())`: Producing the stream was successful.
    /// - `Err(InstallerError)`: An error occurred.
    async fn produce<W: Write, F: FnMut()>(
        &mut self,
        stream: W,
        progress_notify: F,
    ) -> Result<(), Self::Error>;
}

impl<T> StreamProducer for &mut T
where
    T: StreamProducer,
{
    async fn produce<W: Write, F: FnMut()>(
        &mut self,
        out: W,
        progress_notify: F,
    ) -> Result<(), Self::Error> {
        (*self).produce(out, progress_notify).await
    }
}

/// A consumer that reads and discards all data from the incoming stream.
pub struct NoopConsumer;

impl ErrorType for NoopConsumer {
    type Error = ErrorKind;
}

impl StreamConsumer for NoopConsumer {
    async fn consume<R: Read, F: FnMut()>(
        &mut self,
        mut stream: R,
        _progress_notify: F,
    ) -> Result<(), Self::Error> {
        let mut buf = [0u8; 64];

        loop {
            let len = stream.read(&mut buf).await.map_err(|e| e.kind())?;
            if len == 0 {
                break;
            }

            trace!("Consumed {}B: {:?}", len, &buf[..len]);
        }

        Ok(())
    }
}

/// A consumer that reads the incoming stream into a provided byte slice and calls a closure with the data.
pub struct SliceConsumer<'a> {
    buf: &'a mut [u8],
    size: Option<usize>,
}

impl<'a> SliceConsumer<'a> {
    /// Create a new `SliceConsumer`.
    ///
    /// # Arguments
    /// - `buf`: The buffer to read the stream into.
    pub const fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, size: None }
    }

    /// Return the size of the consumed data, if any.
    pub const fn size(&self) -> Option<usize> {
        self.size
    }

    /// Return the consumed data as a byte slice.
    pub fn data(&self) -> &[u8] {
        &self.buf[..self.size.unwrap_or(0)]
    }
}

impl ErrorType for SliceConsumer<'_> {
    type Error = ErrorKind;
}

impl StreamConsumer for SliceConsumer<'_> {
    async fn consume<R: Read, F: FnMut()>(
        &mut self,
        stream: R,
        _progress_notify: F,
    ) -> Result<(), Self::Error> {
        let size = read_all(stream, self.buf, Some(ErrorKind::OutOfMemory))
            .await
            .map_err(|e| e.kind())?;

        self.size = Some(size);

        Ok(())
    }
}

/// A producer that writes the contents of a byte slice into the provided stream.
pub struct SliceProducer<'a> {
    data: &'a [u8],
}

impl<'a> SliceProducer<'a> {
    /// Create a new `SliceProducer`.
    ///
    /// # Arguments
    /// - `data`: The data to write into the stream.
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data }
    }
}

impl ErrorType for SliceProducer<'_> {
    type Error = ErrorKind;
}

impl StreamProducer for SliceProducer<'_> {
    async fn produce<W: Write, F: FnMut()>(
        &mut self,
        mut stream: W,
        _progress_notify: F,
    ) -> Result<(), Self::Error> {
        stream.write_all(self.data).await.map_err(|e| e.kind())?;

        Ok(())
    }
}

/// A factory for wrapping readers.
///
/// Useful for e.g. abstracting an encrypted vs non-encrypted stream.
pub trait ReadWrapper {
    /// The Reader type returned by the wrapper.
    type Reader<'a, R>: Read<Error = ErrorKind>
    where
        Self: 'a,
        R: Read;

    /// Wrap a reader.
    fn wrap<R: Read>(&mut self, read: R) -> Self::Reader<'_, R>;
}

impl<T> ReadWrapper for &mut T
where
    T: ReadWrapper,
{
    type Reader<'a, R>
        = T::Reader<'a, R>
    where
        Self: 'a,
        R: Read;

    fn wrap<R: Read>(&mut self, read: R) -> Self::Reader<'_, R> {
        (*self).wrap(read)
    }
}

/// A factory for wrapping writers.
///
/// Useful for e.g. abstracting an encrypted vs non-encrypted stream.
pub trait WriteWrapper {
    /// The Writer type returned by the wrapper.
    type Writer<'a, W>: Write<Error = ErrorKind>
    where
        Self: 'a,
        W: Write;

    /// Wrap a writer.
    fn wrap<W: Write>(&mut self, write: W) -> Self::Writer<'_, W>;
}

impl<T> WriteWrapper for &mut T
where
    T: WriteWrapper,
{
    type Writer<'a, W>
        = T::Writer<'a, W>
    where
        Self: 'a,
        W: Write;

    fn wrap<W: Write>(&mut self, write: W) -> Self::Writer<'_, W> {
        (*self).wrap(write)
    }
}

/// A no-operation wrapper that returns the original reader/writer unchanged.
pub struct NoopWrapper;

impl ReadWrapper for NoopWrapper {
    type Reader<'a, R>
        = NoopIo<R>
    where
        Self: 'a,
        R: Read;

    fn wrap<R: Read>(&mut self, read: R) -> Self::Reader<'_, R> {
        NoopIo(read)
    }
}

impl WriteWrapper for NoopWrapper {
    type Writer<'a, W>
        = NoopIo<W>
    where
        Self: 'a,
        W: Write;

    fn wrap<W: Write>(&mut self, write: W) -> Self::Writer<'_, W> {
        NoopIo(write)
    }
}

/// A simple enum representing a value that can be one of two types
/// We need our own type so that we can implement external traits on it
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum EitherWrapper<F, S> {
    /// The first type
    First(F),
    /// The second type
    Second(S),
}

impl<F, S> ReadWrapper for EitherWrapper<F, S>
where
    F: ReadWrapper,
    S: ReadWrapper,
{
    type Reader<'a, R>
        = EitherWrapper<F::Reader<'a, R>, S::Reader<'a, R>>
    where
        Self: 'a,
        R: Read;

    fn wrap<R: Read>(&mut self, read: R) -> Self::Reader<'_, R> {
        match self {
            EitherWrapper::First(f) => EitherWrapper::First(f.wrap(read)),
            EitherWrapper::Second(s) => EitherWrapper::Second(s.wrap(read)),
        }
    }
}

impl<F, S> WriteWrapper for EitherWrapper<F, S>
where
    F: WriteWrapper,
    S: WriteWrapper,
{
    type Writer<'a, W>
        = EitherWrapper<F::Writer<'a, W>, S::Writer<'a, W>>
    where
        Self: 'a,
        W: Write;

    fn wrap<W: Write>(&mut self, write: W) -> Self::Writer<'_, W> {
        match self {
            EitherWrapper::First(f) => EitherWrapper::First(f.wrap(write)),
            EitherWrapper::Second(s) => EitherWrapper::Second(s.wrap(write)),
        }
    }
}

impl<F, S> ErrorType for EitherWrapper<F, S> {
    type Error = ErrorKind;
}

impl<F, S> Read for EitherWrapper<F, S>
where
    F: Read,
    S: Read,
{
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        match self {
            EitherWrapper::First(f) => f.read(buf).await.map_err(|e| e.kind()),
            EitherWrapper::Second(s) => s.read(buf).await.map_err(|e| e.kind()),
        }
    }
}

impl<F, S> Write for EitherWrapper<F, S>
where
    F: Write,
    S: Write,
{
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        match self {
            EitherWrapper::First(f) => f.write(buf).await.map_err(|e| e.kind()),
            EitherWrapper::Second(s) => s.write(buf).await.map_err(|e| e.kind()),
        }
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        match self {
            EitherWrapper::First(f) => f.flush().await.map_err(|e| e.kind()),
            EitherWrapper::Second(s) => s.flush().await.map_err(|e| e.kind()),
        }
    }
}

/// A no-operation IO wrapper that simply forwards calls to the underlying IO object.
pub struct NoopIo<T>(T);

impl<T> ErrorType for NoopIo<T>
where
    T: ErrorType,
{
    type Error = ErrorKind;
}

impl<T> Read for NoopIo<T>
where
    T: Read,
{
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.0.read(buf).await.map_err(|e| e.kind())
    }
}

impl<T> Write for NoopIo<T>
where
    T: Write,
{
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.0.write(buf).await.map_err(|e| e.kind())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.0.flush().await.map_err(|e| e.kind())
    }
}

/// A consumer that wraps another consumer, applying a reader wrapper to the input stream.
pub struct WrappingConsumer<C, R> {
    consumer: C,
    wrapper: R,
}

impl<C, R> WrappingConsumer<C, R> {
    /// Create a new `WrappingConsumer`.
    ///
    /// # Arguments
    /// - `consumer`: The underlying consumer to wrap.
    /// - `wrapper`: The reader wrapper to apply to the input stream.
    pub const fn new(consumer: C, wrapper: R) -> Self {
        Self { consumer, wrapper }
    }
}

impl<C, R> ErrorType for WrappingConsumer<C, R>
where
    C: StreamConsumer,
{
    type Error = C::Error;
}

impl<C, R> StreamConsumer for WrappingConsumer<C, R>
where
    C: StreamConsumer,
    R: ReadWrapper,
{
    async fn consume<S: Read, F: FnMut()>(
        &mut self,
        stream: S,
        progress_notify: F,
    ) -> Result<(), Self::Error> {
        self.consumer
            .consume(self.wrapper.wrap(stream), progress_notify)
            .await
    }
}

/// A producer that wraps another producer, applying a writer wrapper to the output stream.
pub struct WrappingProducer<P, W> {
    producer: P,
    wrapper: W,
}

impl<P, W> WrappingProducer<P, W> {
    /// Create a new `WrappingProducer`.
    ///
    /// # Arguments
    /// - `producer`: The underlying producer to wrap.
    /// - `wrapper`: The writer wrapper to apply to the output stream.
    pub const fn new(producer: P, wrapper: W) -> Self {
        Self { producer, wrapper }
    }
}

impl<P, W> ErrorType for WrappingProducer<P, W>
where
    P: StreamProducer,
{
    type Error = P::Error;
}

impl<P, W> StreamProducer for WrappingProducer<P, W>
where
    P: StreamProducer,
    P::Error: From<ErrorKind>,
    W: WriteWrapper,
{
    async fn produce<S: Write, F: FnMut()>(
        &mut self,
        stream: S,
        progress_notify: F,
    ) -> Result<(), Self::Error> {
        let mut stream = self.wrapper.wrap(stream);

        self.producer.produce(&mut stream, progress_notify).await?;

        stream.flush().await?;

        Ok(())
    }
}

/// An error that can occur during serialization or deserialization of a `SerdeObj` instance.
#[derive(thiserror::Error, Debug, Copy, Clone, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum SerdeObjError {
    /// A buffer overflow occurred during serialization.
    #[error("buffer overflow occurred during serialization")]
    BufferOverflow,
    /// The data is invalid during deserialization.
    #[error("invalid data during deserialization")]
    InvalidData,
}

/// A trait for objects that can be serialized and deserialized using Serde from/to JSON.
pub trait SerdeObj<'a>: Serialize + Deserialize<'a> {
    /// The maximum size of the serialized `OnboardingStatus`, as JSON.
    const MAX_BUF_SIZE: usize;

    /// Serialize the object into the provided buffer.
    ///
    /// # Arguments
    /// - `buf`: The buffer to serialize the object into.
    ///
    /// # Returns
    /// - `Ok((payload, remaining_buf))`: The serialized payload and the remaining buffer.
    /// - `Err(SerdeObjError)`: An error occurred during serialization.
    fn serialize<'b>(&self, buf: &'b mut [u8]) -> Result<(&'b [u8], &'b mut [u8]), SerdeObjError> {
        let payload_len =
            serde_json_core::to_slice(self, buf).map_err(|_| SerdeObjError::BufferOverflow)?;
        let (payload, buf) = buf.split_at_mut(payload_len);

        Ok((payload, buf))
    }

    /// Deserialize the object from the provided data.
    ///
    /// # Arguments
    /// - `data`: The data to deserialize the object from.
    ///
    /// # Returns
    /// - `Ok(obj)`: The deserialized object.
    /// - `Err(SerdeObjError)`: An error occurred during deserialization.
    fn deserialize(data: &'a [u8]) -> Result<Self, SerdeObjError> {
        let (obj, obj_len) =
            serde_json_core::from_slice::<Self>(data).map_err(|_| SerdeObjError::InvalidData)?;

        if obj_len != data.len() {
            return Err(SerdeObjError::InvalidData);
        }

        Ok(obj)
    }
}
