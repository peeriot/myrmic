/// Re-exports `postcard::Error`, making `postcard` part of this crate's public
/// API: a consumer mixing two `myrmic-common` majors built against different
/// `postcard` majors gets an unfixable type mismatch.
pub use postcard::Error;
pub use status_code::StatusCode;
pub use url::{Scheme, Url};

mod status_code;
mod url;

pub mod cells;

/// There are two kinds of sessions.
/// 1) Request/Response
/// 2) Websocket
///
/// This information isn't currently exposed, but if there's a need to see if it's a websocket vs req/resp,
/// then it's possible it can be added later.
///
/// This means that there's two separate behaviours for each Outgoing variant that's sent back based on the Session type.
/// Each variant will explain what happens with each session type.
pub type SessionId = uuid::Uuid;

/// A frame flowing from the gateway to the application: a request, a message
/// on an established session, or a session lifecycle notification.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Incoming<'a> {
    /// Data received on an established (websocket) session.
    Message(#[serde(borrow)] Message<'a>),
    /// A session came up.
    ConnectionUp(ConnectionUp),
    /// A session went down.
    ConnectionDown(ConnectionDown),
    /// An HTTP request opening a request/response session.
    Request(#[serde(borrow)] HttpRequest<'a>),
}

impl<'a> Incoming<'a> {
    /// Postcard-encodes the frame into `buffer`, returning the written slice.
    pub fn encode<'b>(&self, buffer: &'b mut [u8]) -> Result<&'b mut [u8], Error> {
        postcard::to_slice(self, buffer)
    }

    /// Postcard-encodes the frame into a freshly allocated vector.
    #[cfg(feature = "alloc")]
    pub fn encode_to_vec(&self) -> Result<alloc::vec::Vec<u8>, Error> {
        postcard::to_allocvec(self)
    }

    /// Postcard-encodes the frame into `writer`.
    pub fn write_to<W>(&self, writer: W) -> Result<W, Error>
    where
        W: Extend<u8>,
    {
        postcard::to_extend(self, writer)
    }

    /// Decodes one frame from the front of `buffer`, returning it and the
    /// unconsumed rest.
    pub fn decode(buffer: &'a [u8]) -> Result<(Self, &'a [u8]), Error> {
        postcard::take_from_bytes(buffer)
    }

    /// The session the frame belongs to.
    pub fn ctx(&self) -> SessionId {
        match self {
            Self::Message(v) => v.ctx,
            Self::ConnectionUp(v) => v.ctx,
            Self::ConnectionDown(v) => v.ctx,
            Self::Request(v) => v.ctx,
        }
    }

    /// Builds a [`Incoming::Message`] frame for `ctx`.
    pub fn message(ctx: SessionId, data: &'a [u8]) -> Self {
        Self::Message(Message { ctx, data })
    }

    /// Builds a [`Incoming::ConnectionUp`] frame for `ctx`.
    pub fn connection_up(ctx: SessionId) -> Self {
        Self::ConnectionUp(ConnectionUp { ctx })
    }

    /// Builds a [`Incoming::ConnectionDown`] frame for `ctx`.
    pub fn connection_down(ctx: SessionId) -> Self {
        Self::ConnectionDown(ConnectionDown { ctx })
    }
}

/// A frame flowing from the application back to the gateway; what each
/// variant does depends on whether the session is request/response or
/// websocket (see [`SessionId`]).
#[allow(clippy::large_enum_variant)]
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outgoing<'a> {
    /// Req: Will attempt to upgrade the request/response session, queueing this message until the upgrade process is complete.
    /// WS: Will send the data directly to the websocket.
    Message(#[serde(borrow)] Message<'a>),
    /// Req: Will send the response directly to the client.
    /// WS: message will be dropped
    Response(#[serde(borrow)] HttpResponse<'a>),
    /// Req: Will resolve the provided path against the database. (you can use this to implement asset lookups, etc)
    /// WS: message will be dropped, lookup is only supported from request/response sessions.
    Lookup(#[serde(borrow)] Lookup<'a>),
    /// Req: Will initiate the upgrade process for a request to a websocket.
    /// WS: message will do nothing. (already upgraded)
    Upgrade(Upgrade),
    /// Req: will respond with a 503 (SERVICE_UNAVAILABLE)
    /// WS: will disconnect the websocket
    Disconnect(Disconnect),
}

impl<'a> Outgoing<'a> {
    /// Postcard-encodes the frame into `buffer`, returning the written slice.
    pub fn encode<'b>(&self, buffer: &'b mut [u8]) -> Result<&'b mut [u8], Error> {
        postcard::to_slice(self, buffer)
    }

    /// Postcard-encodes the frame into a freshly allocated vector.
    #[cfg(feature = "alloc")]
    pub fn encode_to_vec(&self) -> Result<alloc::vec::Vec<u8>, Error> {
        postcard::to_allocvec(self)
    }

    /// Postcard-encodes the frame into an [`embedded_io::Write`] sink.
    #[cfg(feature = "eio")]
    pub fn to_eio<W>(&self, writer: W) -> Result<W, Error>
    where
        W: embedded_io::Write,
    {
        postcard::to_eio(self, writer)
    }

    /// Postcard-encodes the frame into `writer`.
    pub fn write_to<W>(&self, writer: W) -> Result<W, Error>
    where
        W: Extend<u8>,
    {
        postcard::to_extend(self, writer)
    }

    /// Decodes one frame from the front of `buffer`, returning it and the
    /// unconsumed rest.
    pub fn decode(buffer: &'a [u8]) -> Result<(Self, &'a [u8]), Error> {
        postcard::take_from_bytes(buffer)
    }

    /// The session the frame belongs to.
    pub fn ctx(&self) -> SessionId {
        match self {
            Self::Message(v) => v.ctx,
            Self::Response(v) => v.ctx,
            Self::Lookup(v) => v.ctx,
            Self::Upgrade(v) => v.ctx,
            Self::Disconnect(v) => v.ctx,
        }
    }

    /// Builds a [`Outgoing::Message`] frame for `ctx`.
    pub fn message(ctx: SessionId, data: &'a [u8]) -> Self {
        Self::Message(Message { ctx, data })
    }

    /// Wraps `response` as a [`Outgoing::Response`] frame.
    pub fn response(response: HttpResponse<'a>) -> Self {
        Self::Response(response)
    }

    /// Builds a [`Outgoing::Lookup`] frame resolving `path` for `ctx`.
    pub fn lookup(ctx: SessionId, path: &'a str) -> Self {
        Self::Lookup(Lookup { ctx, path })
    }

    /// Builds a [`Outgoing::Upgrade`] frame for `ctx`.
    pub fn upgrade(ctx: SessionId) -> Self {
        Self::Upgrade(Upgrade { ctx })
    }

    /// Builds a [`Outgoing::Disconnect`] frame for `ctx`.
    pub fn disconnect(ctx: SessionId) -> Self {
        Self::Disconnect(Disconnect { ctx })
    }
}

/// Maximum number of headers a request or response frame carries.
pub const HEADER_MAX: usize = 8;

/// The fixed-capacity header list of a request or response frame.
pub type Headers<'a> = heapless::Vec<Header<'a>, HEADER_MAX>;

/// An HTTP request, opening a request/response session.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[allow(missing_docs)]
pub struct HttpRequest<'a> {
    pub ctx: SessionId,

    pub method: Method,

    pub path: &'a str,

    /// The request headers (at most [`HEADER_MAX`]).
    #[serde(borrow)]
    pub headers: Headers<'a>,

    pub body: &'a [u8],
}

/// An HTTP response, closing a request/response session.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[allow(missing_docs)]
pub struct HttpResponse<'a> {
    pub ctx: SessionId,

    pub status_code: StatusCode,

    /// The response headers (at most [`HEADER_MAX`]).
    #[serde(borrow)]
    pub headers: Headers<'a>,

    pub body: &'a [u8],
}

/// One HTTP header as carried on a request or response frame.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[allow(missing_docs)]
pub struct Header<'a> {
    #[serde(borrow)]
    pub name: &'a str,
    #[serde(borrow)]
    pub value: &'a str,
}

#[derive(
    Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[repr(u8)]
#[allow(missing_docs)]
pub enum Method {
    Options,
    Get,
    Post,
    Put,
    Delete,
    Head,
    Trace,
    Connect,
    Patch,
}

/// Data carried on an established session.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[allow(missing_docs)]
pub struct Message<'a> {
    pub ctx: SessionId,

    #[serde(borrow)]
    pub data: &'a [u8],
}

/// Notification that a session came up.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[allow(missing_docs)]
pub struct ConnectionUp {
    pub ctx: SessionId,
}

/// Notification that a session went down.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[allow(missing_docs)]
pub struct ConnectionDown {
    pub ctx: SessionId,
}

/// Request to end a session (see [`Outgoing::Disconnect`]).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[allow(missing_docs)]
pub struct Disconnect {
    pub ctx: SessionId,
}

/// Request to resolve `path` against the database and answer the session with
/// the result (see [`Outgoing::Lookup`]).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[allow(missing_docs)]
pub struct Lookup<'a> {
    pub ctx: SessionId,

    #[serde(borrow)]
    pub path: &'a str,
}

/// Request to upgrade a request/response session to a websocket (see
/// [`Outgoing::Upgrade`]).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[allow(missing_docs)]
pub struct Upgrade {
    pub ctx: SessionId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ctx() {
        let msg = Incoming::Request(HttpRequest {
            ctx: Default::default(),
            method: Method::Options,
            path: "/",
            headers: Default::default(),
            body: b"this is some body",
        });

        let mut buffer = [0u8; 1024];
        let encoded = msg.encode(&mut buffer).unwrap();
        let (value, rest) = Incoming::decode(&*encoded).unwrap();
        assert!(rest.is_empty());
        assert_eq!(msg.ctx(), value.ctx());
    }
}
