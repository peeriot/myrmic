//! This module implements the Zenoh traits for the `zenoh` crate.

use core::borrow::Borrow;

use std::io::{Read as _, Seek as _};

use zenoh::Session;
use zenoh::handlers::FifoChannelHandler;
use zenoh::pubsub::{Publisher, Subscriber};
use zenoh::query::{Query, Queryable, Reply, ReplyError};
use zenoh::sample::Sample;

use crate as traits;

extern crate alloc;
extern crate std;

/// A `zenoh` error implementing the `Error` trait
// TODO: Convert to `thiserror`
#[derive(thiserror::Error, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum ZError {
    /// General error
    #[error("General error: {0}")]
    General(#[from] alloc::boxed::Box<dyn core::error::Error + Send + Sync>),
    /// Reply error
    #[error("Reply error: {0}")]
    Reply(#[from] ReplyError),
    /// IO error
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl<'a> From<&'a ReplyError> for ZError {
    fn from(err: &'a ReplyError) -> Self {
        ZError::Reply(err.clone())
    }
}

impl crate::Error for ZError {
    fn kind(&self) -> crate::ErrorKind {
        crate::ErrorKind::Other
    }
}

/// A `zenoh` session implementing the `Session` trait
pub struct ZSession(Session);

impl ZSession {
    /// Create a new `ZSession` instance
    pub const fn new(session: Session) -> Self {
        Self(session)
    }
}

impl traits::Session for ZSession {
    type Error = ZError;

    type Getter<'a>
        = ZGetter
    where
        Self: 'a;

    type Setter<'a>
        = ZSetter<'a>
    where
        Self: 'a;

    type Publisher<'a>
        = ZPublisher<'a>
    where
        Self: 'a;

    type Subscriber<'a>
        = ZSubscriber
    where
        Self: 'a;

    async fn get<'a>(&'a self, topic: &'a str) -> Result<Self::Getter<'a>, Self::Error> {
        let querier = self.0.declare_querier(topic).await?;

        Ok(ZGetter(querier.get().await.unwrap()))
    }

    async fn set<'a>(&'a self, topic: &'a str) -> Result<Self::Setter<'a>, Self::Error> {
        let queryable = self.0.declare_queryable(topic).await?;

        Ok(ZSetter { queryable, topic })
    }

    async fn publish<'a>(&'a self, topic: &'a str) -> Result<Self::Publisher<'a>, Self::Error> {
        let publisher = self.0.declare_publisher(topic).await?;

        Ok(ZPublisher(publisher))
    }

    async fn subscribe<'a>(&'a self, topic: &'a str) -> Result<Self::Subscriber<'a>, Self::Error> {
        let subscriber = self.0.declare_subscriber(topic).await?;

        Ok(ZSubscriber(subscriber))
    }
}

/// A `zenoh` getter implementing the `Receiver` trait
pub struct ZGetter(FifoChannelHandler<Reply>);

impl traits::ErrorType for ZGetter {
    type Error = ZError;
}

impl traits::Close for ZGetter {
    async fn close(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl traits::Receiver for ZGetter {
    type Read<'a>
        = ZReplyRead
    where
        Self: 'a;

    async fn receive(&mut self) -> Result<(&str, Self::Read<'_>), Self::Error> {
        let reply = self.0.recv_async().await?;

        Ok(("", ZReplyRead { reply, pos: 0 }))
    }
}

/// A `zenoh` setter implementing the `Sender` trait
pub struct ZSetter<'a> {
    queryable: Queryable<FifoChannelHandler<Query>>,
    topic: &'a str,
}

impl traits::ErrorType for ZSetter<'_> {
    type Error = ZError;
}

impl traits::Close for ZSetter<'_> {
    async fn close(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl traits::Sender for ZSetter<'_> {
    type SendPayload<'a>
        = ZSetterSendPayload<'a>
    where
        Self: 'a;

    async fn send(&mut self) -> Result<Self::SendPayload<'_>, Self::Error> {
        let query = self.queryable.recv_async().await?;

        Ok(ZSetterSendPayload {
            setter: self,
            payload: Some(alloc::vec::Vec::new()),
            query: Some(query),
        })
    }
}

/// A `zenoh` subscriber implementing the `Receiver` trait
pub struct ZSubscriber(Subscriber<FifoChannelHandler<Sample>>);

impl traits::ErrorType for ZSubscriber {
    type Error = ZError;
}

impl traits::Close for ZSubscriber {
    async fn close(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl traits::Receiver for ZSubscriber {
    type Read<'a>
        = ZSampleRead<Sample>
    where
        Self: 'a;

    async fn receive(&mut self) -> Result<(&str, Self::Read<'_>), Self::Error> {
        let sample = self.0.recv_async().await?;

        Ok(("", ZSampleRead { sample, pos: 0 }))
    }
}

/// A `zenoh` publisher implementing the `Sender` trait
pub struct ZPublisher<'a>(Publisher<'a>);

impl traits::ErrorType for ZPublisher<'_> {
    type Error = ZError;
}

impl traits::Close for ZPublisher<'_> {
    async fn close(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<'t> traits::Sender for ZPublisher<'t> {
    type SendPayload<'a>
        = ZPublisherSendPayload<'a, 't>
    where
        Self: 'a;

    async fn send(&mut self) -> Result<Self::SendPayload<'_>, Self::Error> {
        Ok(ZPublisherSendPayload {
            publisher: self,
            payload: Some(alloc::vec::Vec::new()),
        })
    }
}

/// A `zenoh` reply reader implementing the `Read` trait
/// Used by the `ZGetter` type
pub struct ZReplyRead {
    reply: Reply,
    pos: usize,
}

impl traits::ErrorType for ZReplyRead {
    type Error = ZError;
}

impl traits::Close for ZReplyRead {
    async fn close(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl traits::Read for ZReplyRead {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let sample = self.reply.result()?;

        let mut reader = sample.payload().reader();

        reader.seek(std::io::SeekFrom::Start(self.pos as _))?;
        let len = reader.read(buf)?;

        self.pos += len;

        Ok(len)
    }
}

/// A `zenoh` sample reader implementing the `Read` trait
/// Used by the `ZSubscriber` type
pub struct ZSampleRead<T> {
    sample: T,
    pos: usize,
}

impl<T: Borrow<Sample>> traits::ErrorType for ZSampleRead<T> {
    type Error = ZError;
}

impl<T: Borrow<Sample>> traits::Close for ZSampleRead<T> {
    async fn close(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<T: Borrow<Sample>> traits::Read for ZSampleRead<T> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let sample = self.sample.borrow();

        let mut reader = sample.payload().reader();

        reader.seek(std::io::SeekFrom::Start(self.pos as _))?;
        let len = reader.read(buf)?;

        self.pos += len;

        Ok(len)
    }
}

/// A type for implementing the `SendPayload` and `Write` traits for `zenoh`
/// Used by the `ZSetter` type
pub struct ZSetterSendPayload<'a> {
    setter: &'a ZSetter<'a>,
    query: Option<Query>,
    payload: Option<alloc::vec::Vec<u8>>,
}

impl traits::ErrorType for ZSetterSendPayload<'_> {
    type Error = ZError;
}

impl traits::Close for ZSetterSendPayload<'_> {
    async fn close(&mut self) -> Result<(), Self::Error> {
        if let Some(payload) = self.payload.take() {
            self.query
                .take()
                .unwrap()
                .reply(self.setter.topic, payload.as_slice())
                .await?;
        }

        Ok(())
    }
}

impl traits::Write for ZSetterSendPayload<'_> {
    async fn write(&mut self, data: &[u8]) -> Result<usize, Self::Error> {
        if let Some(payload) = &mut self.payload {
            payload.extend_from_slice(data);
        } else {
            panic!("Stream is already closed");
        }

        Ok(data.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<'a> traits::SendPayload<'a> for ZSetterSendPayload<'a> {
    type Write = Self;

    async fn with_encoding(self, _encoding: &str) -> Result<Self::Write, Self::Error> {
        Ok(self)
    }
}

impl Drop for ZSetterSendPayload<'_> {
    fn drop(&mut self) {
        if self.payload.is_some() {
            warn!("ZSetterSendPayload dropped without being closed!");
            embassy_futures::block_on(traits::Close::close(self)).unwrap();
        }
    }
}

/// A type for implementing the `SendPayload` and `Write` traits for `zenoh`
/// Used by the `ZPublisher` type
pub struct ZPublisherSendPayload<'a, 't> {
    publisher: &'a mut ZPublisher<'t>,
    payload: Option<alloc::vec::Vec<u8>>,
}

impl traits::ErrorType for ZPublisherSendPayload<'_, '_> {
    type Error = ZError;
}

impl traits::Close for ZPublisherSendPayload<'_, '_> {
    async fn close(&mut self) -> Result<(), Self::Error> {
        if let Some(payload) = &mut self.payload {
            self.publisher.0.put(payload.as_slice()).await?;
            self.payload = None;
        }

        Ok(())
    }
}

impl traits::Write for ZPublisherSendPayload<'_, '_> {
    async fn write(&mut self, data: &[u8]) -> Result<usize, Self::Error> {
        if let Some(payload) = &mut self.payload {
            payload.extend_from_slice(data);
        } else {
            panic!("Stream is already closed");
        }

        Ok(data.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<'a> traits::SendPayload<'a> for ZPublisherSendPayload<'a, '_> {
    type Write = Self;

    async fn with_encoding(self, _encoding: &str) -> Result<Self::Write, Self::Error> {
        Ok(self)
    }
}

impl Drop for ZPublisherSendPayload<'_, '_> {
    fn drop(&mut self) {
        if self.payload.is_some() {
            warn!("ZPublisherSendPayload dropped without being closed!");
            embassy_futures::block_on(traits::Close::close(self)).unwrap();
        }
    }
}
