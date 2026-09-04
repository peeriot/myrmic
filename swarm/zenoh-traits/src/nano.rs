//! This module implements the Zenoh traits for the `zenoh-nano` crate.

use core::num::NonZeroUsize;

use alloc::vec::Vec;

use embassy_sync::blocking_mutex::raw::{NoopRawMutex, RawMutex};

use zenoh_nano::buffers::ZBuf;
use zenoh_nano::buffers::buffer::Buffer as _;
use zenoh_nano::buffers::reader::{AdvanceableReader, HasReader as _, Reader};
use zenoh_nano::link::LinkError;
use zenoh_nano::network::NetworkError;
use zenoh_nano::ops::get::Get;
use zenoh_nano::ops::publish::Publisher;
use zenoh_nano::ops::queryable::{Query, Queryable};
use zenoh_nano::ops::subscribe::Subscriber;
use zenoh_nano::session::{Session, SessionError};
use zenoh_nano::transport::TransportError;

extern crate alloc;

use crate as traits;

/// Encodings do not seem to be supported by `zenoh-nano`
const UNKNOWN_ENCODING: &str = "";

/// A `zenoh-nano` error implementing the `Error` trait
// TODO: Convert to `thiserror`
#[derive(thiserror::Error, Debug, Copy, Clone, Eq, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[error("Zenoh-Nano Error: {0}")]
pub struct ZNError(#[from] SessionError);

impl traits::Error for ZNError {
    fn kind(&self) -> traits::ErrorKind {
        match self.0 {
            SessionError::NoPublishCapacity | SessionError::NoDispatcherCapacity => {
                traits::ErrorKind::OutOfMemory
            }
            SessionError::Network(e) => match e {
                NetworkError::OutgoingMessageEncoding => traits::ErrorKind::InvalidData,
                NetworkError::Transport(e) => match e {
                    TransportError::RngTimeout | TransportError::SessionNegoTimeout => {
                        traits::ErrorKind::TimedOut
                    }
                    TransportError::SessionNegoInvalidResponse
                    | TransportError::IncomingMessageInvalid
                    | TransportError::OutgoingMessageTooLarge
                    | TransportError::OutgoingMessageEncoding => traits::ErrorKind::InvalidData,
                    TransportError::Link(e) => match e {
                        LinkError::PayloadTooLarge => traits::ErrorKind::InvalidData,
                        LinkError::UnexpectedEof => traits::ErrorKind::NotConnected,
                        LinkError::Io(e) => e.kind(),
                    },
                },
            },
        }
    }
}

/// A `zenoh-nano`` session implementing the `Session` trait
pub struct ZNSession<'a, M: RawMutex = NoopRawMutex>(Session<'a, M>);

impl<'a, M: RawMutex> ZNSession<'a, M> {
    /// Create a new `ZNSession` instance
    pub const fn new(session: Session<'a, M>) -> Self {
        Self(session)
    }
}

impl<M: RawMutex> traits::Session for ZNSession<'_, M> {
    type Error = ZNError;

    type Getter<'a>
        = ZNGetter<'a, M>
    where
        Self: 'a;

    type Setter<'a>
        = ZNSetter<'a, M>
    where
        Self: 'a;

    type Publisher<'a>
        = ZNPublisher<'a>
    where
        Self: 'a;

    type Subscriber<'a>
        = ZNSubscriber<'a>
    where
        Self: 'a;

    async fn get<'a>(&'a self, topic: &'a str) -> Result<Self::Getter<'a>, Self::Error> {
        Ok(ZNGetter {
            session: self.0,
            topic,
        })
    }

    async fn set<'a>(&'a self, topic: &'a str) -> Result<Self::Setter<'a>, Self::Error> {
        let queryable = Queryable::declare(self.0, topic).await?;

        Ok(ZNSetter(queryable))
    }

    async fn publish<'a>(&'a self, topic: &'a str) -> Result<Self::Publisher<'a>, Self::Error> {
        let publisher = Publisher::declare(self.0, topic).await?;

        Ok(ZNPublisher(publisher))
    }

    async fn subscribe<'a>(&'a self, topic: &'a str) -> Result<Self::Subscriber<'a>, Self::Error> {
        let subscriber = Subscriber::declare(self.0, topic).await?;

        Ok(ZNSubscriber(subscriber))
    }
}

/// A `zenoh-nano` getter implementing the `Receiver` trait
pub struct ZNGetter<'a, M: RawMutex> {
    session: Session<'a, M>,
    topic: &'a str,
}

impl<M: RawMutex> traits::ErrorType for ZNGetter<'_, M> {
    type Error = ZNError;
}

impl<M: RawMutex> traits::Close for ZNGetter<'_, M> {
    async fn close(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<M: RawMutex> traits::Receiver for ZNGetter<'_, M> {
    type Read<'a>
        = ZNBufRead
    where
        Self: 'a;

    async fn receive(&mut self) -> Result<(&str, Self::Read<'_>), Self::Error> {
        let zbuf = match Get::new(self.session, self.topic).await? {
            zenoh_nano::ops::get::GetResult::Ok(z) | zenoh_nano::ops::get::GetResult::Err(z) => z,
            // `ZNError` only wraps `SessionError`, which has no timeout variant,
            // so these keep reading as an empty payload — the behaviour this
            // impl already had before the outcomes were named.
            zenoh_nano::ops::get::GetResult::Timeout | zenoh_nano::ops::get::GetResult::NoReply => {
                zenoh_nano::buffers::ZBuf::empty()
            }
        };

        Ok((UNKNOWN_ENCODING, ZNBufRead { zbuf, pos: 0 }))
    }
}

/// A `zenoh-nano` setter implementing the `Sender` trait
pub struct ZNSetter<'a, M: RawMutex = NoopRawMutex>(Queryable<'a, M>);

impl<M: RawMutex> traits::ErrorType for ZNSetter<'_, M> {
    type Error = ZNError;
}

impl<M: RawMutex> traits::Close for ZNSetter<'_, M> {
    async fn close(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<'t, M: RawMutex> traits::Sender for ZNSetter<'t, M> {
    type SendPayload<'a>
        = ZNSetterSendPayload<'a, 't, M>
    where
        Self: 'a;

    async fn send(&mut self) -> Result<Self::SendPayload<'_>, Self::Error> {
        let query = self.0.wait_for_query().await?;

        Ok(ZNSetterSendPayload {
            setter: self,
            payload: Some(ZBuf::empty()),
            query: Some(query),
        })
    }
}

/// A `zenoh-nano` subscriber implementing the `Receiver` trait
pub struct ZNSubscriber<'a>(Subscriber<'a, &'a str>);

impl traits::ErrorType for ZNSubscriber<'_> {
    type Error = ZNError;
}

impl traits::Close for ZNSubscriber<'_> {
    async fn close(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl traits::Receiver for ZNSubscriber<'_> {
    type Read<'a>
        = ZNBufRead
    where
        Self: 'a;

    async fn receive(&mut self) -> Result<(&str, Self::Read<'_>), Self::Error> {
        Ok((
            UNKNOWN_ENCODING,
            ZNBufRead {
                zbuf: self.0.receive().await?,
                pos: 0,
            },
        ))
    }
}

/// A `zenoh-nano` publisher implementing the `Sender` trait
pub struct ZNPublisher<'a>(Publisher<'a, &'a str>);

impl traits::ErrorType for ZNPublisher<'_> {
    type Error = ZNError;
}

impl traits::Close for ZNPublisher<'_> {
    async fn close(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<'t> traits::Sender for ZNPublisher<'t> {
    type SendPayload<'a>
        = ZNPublisherSendPayload<'a, 't>
    where
        Self: 'a;

    async fn send(&mut self) -> Result<Self::SendPayload<'_>, Self::Error> {
        Ok(ZNPublisherSendPayload {
            publisher: self,
            payload: Some(ZBuf::empty()),
        })
    }
}

/// A type for implementing the `SendPayload` and `Write` traits for `zenoh-nano`
/// Used by the `ZNSetter` type
pub struct ZNSetterSendPayload<'a, 't, M: RawMutex = NoopRawMutex> {
    setter: &'a mut ZNSetter<'t, M>,
    query: Option<Query>,
    payload: Option<ZBuf>,
}

impl<M: RawMutex> traits::ErrorType for ZNSetterSendPayload<'_, '_, M> {
    type Error = ZNError;
}

impl<M: RawMutex> traits::Close for ZNSetterSendPayload<'_, '_, M> {
    async fn close(&mut self) -> Result<(), Self::Error> {
        if let Some(payload) = self.payload.take() {
            self.setter
                .0
                .reply_to_query(self.query.take().unwrap(), Ok(payload))
                .await?;
        }

        Ok(())
    }
}

impl<M: RawMutex> traits::Write for ZNSetterSendPayload<'_, '_, M> {
    async fn write(&mut self, data: &[u8]) -> Result<usize, Self::Error> {
        if let Some(payload) = &mut self.payload {
            payload.push_zslice(Vec::from(data).into());
        } else {
            panic!("Stream is already closed");
        }

        Ok(data.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<'a, 't, M: RawMutex> traits::SendPayload<'a> for ZNSetterSendPayload<'a, 't, M> {
    type Write = Self;

    async fn with_encoding(self, _encoding: &str) -> Result<Self::Write, Self::Error> {
        Ok(self)
    }
}

impl<M: RawMutex> Drop for ZNSetterSendPayload<'_, '_, M> {
    fn drop(&mut self) {
        if self.payload.is_some() {
            warn!("ZNSetterSendPayload dropped without being closed!");
            embassy_futures::block_on(traits::Close::close(self)).unwrap();
        }
    }
}

/// A type for implementing the `SendPayload` and `Write` traits for `zenoh-nano`
/// Used by the `ZNPublisher` type
pub struct ZNPublisherSendPayload<'a, 't> {
    publisher: &'a mut ZNPublisher<'t>,
    payload: Option<ZBuf>,
}

impl traits::ErrorType for ZNPublisherSendPayload<'_, '_> {
    type Error = ZNError;
}

impl traits::Close for ZNPublisherSendPayload<'_, '_> {
    async fn close(&mut self) -> Result<(), Self::Error> {
        if let Some(payload) = self.payload.take() {
            self.publisher.0.publish(payload).await?;
        }

        Ok(())
    }
}

impl traits::Write for ZNPublisherSendPayload<'_, '_> {
    async fn write(&mut self, data: &[u8]) -> Result<usize, Self::Error> {
        if let Some(payload) = &mut self.payload {
            payload.push_zslice(Vec::from(data).into());
        } else {
            panic!("Stream is already closed");
        }

        Ok(data.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<'a, 't> traits::SendPayload<'a> for ZNPublisherSendPayload<'a, 't> {
    type Write = Self;

    async fn with_encoding(self, _encoding: &str) -> Result<Self::Write, Self::Error> {
        Ok(self)
    }
}

impl Drop for ZNPublisherSendPayload<'_, '_> {
    fn drop(&mut self) {
        if self.payload.is_some() {
            warn!("ZNPublisherSendPayload dropped without being closed!");
            embassy_futures::block_on(traits::Close::close(self)).unwrap();
        }
    }
}

/// A type for implementing the `Read` and `Close` traits over a `ZBuf` buffer
/// Used by the `ZNGetter` and `ZNSubscriber` types
pub struct ZNBufRead {
    /// The owned ZBuf instance
    zbuf: ZBuf,
    /// The current position within the Vec instance
    pos: usize,
}

impl traits::ErrorType for ZNBufRead {
    type Error = ZNError;
}

impl traits::Close for ZNBufRead {
    async fn close(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl traits::Read for ZNBufRead {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if self.pos >= self.zbuf.len() {
            return Ok(0);
        }

        let mut reader = self.zbuf.reader();
        if reader.skip(self.pos).is_err() {
            return Ok(0);
        }

        let len = reader.read(buf).map(NonZeroUsize::get).unwrap_or(0);
        self.pos += len;

        Ok(len)
    }
}
