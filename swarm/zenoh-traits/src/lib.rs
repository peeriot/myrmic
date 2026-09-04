//! Traits abstracting over a Zenoh session and its operations.

#![no_std]
#![deny(missing_docs)]
#![allow(async_fn_in_trait)]
#![allow(clippy::uninlined_format_args)] // For `defmt`

pub use embedded_io_async::{Error, ErrorKind, ErrorType, Read, Write};

// Brings the log-level, assert and `unwrap!` macros into crate-wide scope, the way
// the vendored `fmt` module used to. Both modules that use them are feature-gated,
// so this is unused when neither is enabled.
#[allow(unused_imports)]
#[macro_use]
extern crate defmt_or_log;

#[cfg(feature = "zenoh-nano")]
pub mod nano;
#[cfg(feature = "zenoh")]
pub mod zenoh;

/// A Zenoh session
pub trait Session {
    /// The error type for this session
    type Error: Error;

    /// The Getter associated type
    type Getter<'a>: Receiver<Error = Self::Error>
    where
        Self: 'a;

    /// The Setter associated type
    type Setter<'a>: Sender<Error = Self::Error>
    where
        Self: 'a;

    /// The Publisher associated type
    type Publisher<'a>: Sender<Error = Self::Error>
    where
        Self: 'a;

    /// The Subscriber associated type
    type Subscriber<'a>: Receiver<Error = Self::Error>
    where
        Self: 'a;

    /// Create a Getter for the specified topic
    ///
    /// A Getter allows to repeatedly perform the Zenoh `get` verb on the specified topic.
    ///
    /// Arguments:
    /// - `topic`: The topic (key) to perform `get` on
    ///
    /// Returns:
    /// - `Ok(Getter)`: A Getter instance to perform `get` operations
    /// - `Err(Error)`: An error if the Getter could not be created
    async fn get<'a>(&'a self, topic: &'a str) -> Result<Self::Getter<'a>, Self::Error>;

    /// Create a Setter for the specified topic
    ///
    /// A Setter allows to reply to `get` verbs issued against the specified topic.
    ///
    /// Arguments:
    /// - `topic`: The topic (key) to perform `set` on
    ///
    /// Returns:
    /// - `Ok(Setter)`: A Setter instance to perform `set` operations
    /// - `Err(Error)`: An error if the Setter could not be created
    async fn set<'a>(&'a self, topic: &'a str) -> Result<Self::Setter<'a>, Self::Error>;

    /// Create a Publisher for the specified topic
    ///
    /// A Publisher allows to publish data to the specified topic.
    ///
    /// Arguments:
    /// - `topic`: The topic (key) to publish data to
    ///
    /// Returns:
    /// - `Ok(Publisher)`: A Publisher instance to perform `publish` operations
    /// - `Err(Error)`: An error if the Publisher could not be created
    async fn publish<'a>(&'a self, topic: &'a str) -> Result<Self::Publisher<'a>, Self::Error>;

    /// Create a Subscriber for the specified topic
    ///
    /// A Subscriber allows to receive data published to the specified topic.
    ///
    /// Arguments:
    /// - `topic`: The topic (key) to subscribe to
    ///
    /// Returns:
    /// - `Ok(Subscriber)`: A Subscriber instance to perform `subscribe` operations
    /// - `Err(Error)`: An error if the Subscriber could not be created
    async fn subscribe<'a>(&'a self, topic: &'a str) -> Result<Self::Subscriber<'a>, Self::Error>;
}

impl<T> Session for &T
where
    T: Session,
{
    type Error = T::Error;

    type Getter<'a>
        = T::Getter<'a>
    where
        Self: 'a;
    type Setter<'a>
        = T::Setter<'a>
    where
        Self: 'a;
    type Publisher<'a>
        = T::Publisher<'a>
    where
        Self: 'a;
    type Subscriber<'a>
        = T::Subscriber<'a>
    where
        Self: 'a;

    async fn get<'a>(&'a self, topic: &'a str) -> Result<Self::Getter<'a>, Self::Error> {
        (*self).get(topic).await
    }

    async fn set<'a>(&'a self, topic: &'a str) -> Result<Self::Setter<'a>, Self::Error> {
        (*self).set(topic).await
    }

    async fn publish<'a>(&'a self, topic: &'a str) -> Result<Self::Publisher<'a>, Self::Error> {
        (*self).publish(topic).await
    }

    async fn subscribe<'a>(&'a self, topic: &'a str) -> Result<Self::Subscriber<'a>, Self::Error> {
        (*self).subscribe(topic).await
    }
}

/// Types implementing this trait can be closed.
///
/// Explicit closing is useful when IO needs to be performed on the type
/// which is being closed, because Rust does not have an async drop (yet?).
pub trait Close: ErrorType {
    /// Close the type, performing any necessary IO operations.
    async fn close(&mut self) -> Result<(), Self::Error>;
}

impl<T> Close for &mut T
where
    T: Close,
{
    async fn close(&mut self) -> Result<(), Self::Error> {
        (*self).close().await
    }
}

/// A writer that can be closed.
pub trait ClosableWrite: Write + Close {}

impl<T> ClosableWrite for T where T: Write + Close {}

/// A reader that can be closed.
pub trait ClosableRead: Read + Close {}

impl<T> ClosableRead for T where T: Read + Close {}

/// A payload that can be sent.
pub trait SendPayload<'a>: Close {
    /// The writer type to write the payload to.
    type Write: ClosableWrite + 'a;

    /// Send the encoding of the following payload.
    ///
    /// Arguments:
    /// - `encoding`: The encoding to send.
    ///
    /// Returns:
    /// - `Ok(Write)`: A writer to write the payload to.
    /// - `Err(Error)`: An error if the content type could not be sent.
    async fn with_encoding(self, encoding: &str) -> Result<Self::Write, Self::Error>;
}

/// A sender that can send Zenoh payloads. Used for `set` and `publish`.
pub trait Sender: Close {
    /// The payload type that can be sent.
    type SendPayload<'a>: SendPayload<'a>
    where
        Self: 'a;

    /// Send a new payload.
    ///
    /// Returns:
    /// - `Ok(SendPayload)`: A payload to send data to.
    /// - `Err(Error)`: An error if the payload could not be created.
    async fn send(&mut self) -> Result<Self::SendPayload<'_>, Self::Error>;
}

impl<T> Sender for &mut T
where
    T: Sender,
{
    type SendPayload<'a>
        = T::SendPayload<'a>
    where
        Self: 'a;

    async fn send(&mut self) -> Result<Self::SendPayload<'_>, Self::Error> {
        (*self).send().await
    }
}

/// A receiver that can receive Zenoh payloads. Used for `get` and `subscribe`.
pub trait Receiver: Close {
    /// A reader for the payload.
    type Read<'a>: ClosableRead
    where
        Self: 'a;

    /// Receive a payload.
    ///
    /// Returns:
    /// - `Ok((&str, Read))`: The encoding of the payload and a reader to read the payload from.
    /// - `Err(Error)`: An error if the payload could not be received.
    async fn receive(&mut self) -> Result<(&str, Self::Read<'_>), Self::Error>;
}

impl<T> Receiver for &mut T
where
    T: Receiver,
{
    type Read<'a>
        = T::Read<'a>
    where
        Self: 'a;

    async fn receive(&mut self) -> Result<(&str, Self::Read<'_>), Self::Error> {
        (*self).receive().await
    }
}
