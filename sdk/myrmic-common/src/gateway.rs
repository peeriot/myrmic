//! Wire types for the socket gateway: the route a cell mounts, and the frames
//! that carry HTTP requests and `WebSocket` traffic between gateway and cell.
//!
//! A cell asks the host to serve it at a URL prefix, naming the command every
//! request under that prefix is delivered to. The host records the route
//! against the *calling* cell's SRI, so the route's lifetime is the cell's — it
//! is torn down when the cell is undeployed, and gateways drop routes whose
//! owner has left the cell registry.
//!
//! The gateway sends each request as an [`HttpRequest`]. Whoever holds it
//! answers by sending a [`GatewayFrame`] to the request's `gateway` SRI under
//! [`GATEWAY_COMMAND`]. All frames are postcard-encoded.

use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::cells::{Command, Sri};
use crate::db::Scope;
use crate::types::web::StatusCode;

/// A cell's request to be served at `mount`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountRequest {
    /// URL prefix to serve under, e.g. `/chat`. Gateways match the longest
    /// registered prefix, so mounts may nest.
    pub mount: String,
    /// The command every request under `mount` is delivered to, as an
    /// [`HttpRequest`].
    pub http: Command,
}

/// A cell's request to drop one of its mounts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct UnmountRequest {
    pub mount: String,
}

/// The mount path is empty, or not a valid URL prefix.
pub const GATEWAY_ERR_INVALID_MOUNT: core::ffi::c_int = -30;
/// Another cell already owns this mount.
pub const GATEWAY_ERR_MOUNT_TAKEN: core::ffi::c_int = -31;
/// The route registry could not be written.
pub const GATEWAY_ERR_WRITE_FAILED: core::ffi::c_int = -32;
/// No such mount is owned by this cell.
pub const GATEWAY_ERR_NOT_FOUND: core::ffi::c_int = -33;

/// Why a [`MountRequest`] or [`UnmountRequest`] failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayError {
    /// The mount path is empty, or not a valid URL prefix.
    InvalidMount,
    /// Another cell already owns this mount.
    MountTaken,
    /// The route registry could not be written.
    WriteFailed,
    /// No such mount is owned by this cell.
    NotFound,
}

impl TryFrom<core::ffi::c_int> for GatewayError {
    type Error = core::ffi::c_int;

    fn try_from(code: core::ffi::c_int) -> Result<Self, Self::Error> {
        match code {
            GATEWAY_ERR_INVALID_MOUNT => Ok(Self::InvalidMount),
            GATEWAY_ERR_MOUNT_TAKEN => Ok(Self::MountTaken),
            GATEWAY_ERR_WRITE_FAILED => Ok(Self::WriteFailed),
            GATEWAY_ERR_NOT_FOUND => Ok(Self::NotFound),
            _ => Err(code),
        }
    }
}

impl From<GatewayError> for &'static str {
    fn from(err: GatewayError) -> Self {
        match err {
            GatewayError::InvalidMount => "invalid mount path",
            GatewayError::MountTaken => "mount already owned by another cell",
            GatewayError::WriteFailed => "failed to write gateway route",
            GatewayError::NotFound => "mount not found",
        }
    }
}

/// The command a gateway receives [`GatewayFrame`]s on.
pub const GATEWAY_COMMAND: &str = "gateway_frame";

/// Identifies one HTTP request on a gateway. A request upgraded to a
/// `WebSocket` keeps its id for the socket's lifetime.
pub type SessionId = Uuid;

/// An HTTP request arriving under a cell's mount, borrowing its strings and
/// body from the buffer it was decoded from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpRequest<'a> {
    /// The gateway holding the connection; replies go here.
    pub gateway: Sri,
    /// This request, on that gateway.
    pub ctx: SessionId,
    #[allow(missing_docs)]
    pub method: Method,
    /// The request path relative to the mount, always starting with `/`.
    pub path: &'a str,
    /// The raw query string, without the leading `?`.
    #[serde(borrow)]
    pub query: Option<&'a str>,
    /// Headers whose values are valid UTF-8, in arrival order.
    #[serde(borrow)]
    pub headers: Vec<Header<'a>>,
    #[allow(missing_docs)]
    pub body: &'a [u8],
}

impl HttpRequest<'_> {
    /// The first header named `name`, compared case-insensitively.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case(name))
            .map(|header| &*header.value)
    }
}

/// One HTTP header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct Header<'a> {
    #[serde(borrow)]
    pub name: Cow<'a, str>,
    #[serde(borrow)]
    pub value: Cow<'a, str>,
}

impl Header<'_> {
    /// Copies out anything borrowed.
    #[must_use]
    pub fn into_owned(self) -> Header<'static> {
        Header {
            name: Cow::Owned(self.name.into_owned()),
            value: Cow::Owned(self.value.into_owned()),
        }
    }
}

#[derive(
    Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize,
)]
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

/// A complete HTTP response, sent to the client as is.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct HttpResponse {
    pub status: StatusCode,
    #[serde(deserialize_with = "owned_headers")]
    pub headers: Vec<Header<'static>>,
    pub body: Vec<u8>,
}

fn owned_headers<'de, D>(deserializer: D) -> Result<Vec<Header<'static>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let headers = Vec::<Header<'de>>::deserialize(deserializer)?;
    Ok(headers.into_iter().map(Header::into_owned).collect())
}

impl HttpResponse {
    /// An empty response with `status`.
    #[must_use]
    pub fn new(status: StatusCode) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// Appends a header.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push(Header {
            name: Cow::Owned(name.into()),
            value: Cow::Owned(value.into()),
        });
        self
    }

    /// Replaces the body.
    #[must_use]
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }
}

/// Asks the gateway to answer with a blob from the store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    /// The blob path to serve, e.g. the request's own path.
    pub path: String,
    /// Blob scope to read from. `None` uses the replying cell's own asset
    /// scope, the one [`asset_scope`] names. Resolved against the replying
    /// cell, under the same rules as its db calls.
    pub scope: Option<Scope>,
    /// Document served for `/` and directory paths, and as the
    /// [`Fallback::Spa`] target.
    pub index: Option<String>,
    /// What to serve when `path` matches no blob.
    pub fallback: Fallback,
}

impl Asset {
    /// Serves `path` from the replying cell's own asset scope, with no index.
    #[must_use]
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            scope: None,
            index: None,
            fallback: Fallback::default(),
        }
    }

    /// Reads from `scope` instead of the replying cell's asset scope.
    #[must_use]
    pub fn scope(mut self, scope: Scope) -> Self {
        self.scope = Some(scope);
        self
    }

    /// Serves `index` for `/` and directory paths, and as the SPA fallback.
    #[must_use]
    pub fn index(mut self, index: impl Into<String>) -> Self {
        self.index = Some(index.into());
        self
    }

    /// What to serve when the path matches no blob.
    #[must_use]
    pub fn fallback(mut self, fallback: Fallback) -> Self {
        self.fallback = fallback;
        self
    }
}

/// What to serve when a requested asset path matches no blob.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Fallback {
    /// Serve the index document for extensionless paths, so a single-page app
    /// can route client-side. The default.
    #[default]
    Spa,
    /// Return 404.
    None,
}

/// How a cell answers an [`HttpRequest`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HttpReply {
    /// Send this response.
    Response(HttpResponse),
    /// Serve a blob.
    Asset(Asset),
    /// Accept the `WebSocket` upgrade, delivering its [`WsEvent`]s to
    /// `callback` on the replying cell.
    Upgrade {
        #[allow(missing_docs)]
        callback: Command,
    },
}

/// An upgraded `WebSocket` connection on a gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Socket {
    /// The gateway holding the connection.
    pub gateway: Sri,
    /// The id of the request that was upgraded.
    pub ctx: SessionId,
}

/// One `WebSocket` data frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(missing_docs)]
pub enum WsMessage {
    Text(String),
    Binary(Vec<u8>),
}

/// What happened on a [`Socket`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WsEvent {
    /// The upgrade completed; the socket can be sent to.
    Open(Socket),
    /// The client sent a frame.
    Message(Socket, WsMessage),
    /// The socket is gone. Nothing more arrives on it, and sends are dropped.
    Closed(Socket),
}

/// A frame a cell sends to a gateway under [`GATEWAY_COMMAND`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GatewayFrame {
    /// Answers the request `ctx`. Only the first reply counts.
    Reply {
        #[allow(missing_docs)]
        ctx: SessionId,
        #[allow(missing_docs)]
        reply: HttpReply,
    },
    /// Sends a frame on the socket `ctx`.
    Send {
        #[allow(missing_docs)]
        ctx: SessionId,
        #[allow(missing_docs)]
        message: WsMessage,
    },
    /// Closes the socket `ctx`.
    Close {
        #[allow(missing_docs)]
        ctx: SessionId,
    },
}

/// The gateway's namespace: the routing table every gateway serves from, and
/// the static assets cells publish through it.
///
/// Replicated by every node, so any gateway can serve any application.
pub const NAMESPACE_GATEWAY: &str = "gw";

/// Database component of the scope a cell's gateway assets live in.
pub const ASSETS_DB: &str = "gateway-assets";

/// The blob scope a cell serves its own gateway assets from, isolated per cell
/// by the schema component. Write assets here; an [`Asset`] reply with no
/// explicit scope reads from the very same scope.
///
/// This is the one scope in [`NAMESPACE_GATEWAY`] a cell may write: the host
/// checks the schema against the calling cell's own identity, so a cell can
/// neither touch another's assets nor the routing table.
#[must_use]
pub fn asset_scope(sri: &str) -> Scope {
    Scope::public_owned(
        String::from(NAMESPACE_GATEWAY),
        Some(String::from(ASSETS_DB)),
        Some(String::from(sri)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let frame = GatewayFrame::Reply {
            ctx: Uuid::from_u128(7),
            reply: HttpReply::Upgrade {
                callback: Command::new(String::from("on_ws")).unwrap(),
            },
        };
        let bytes = postcard::to_allocvec(&frame).unwrap();
        let GatewayFrame::Reply { ctx, reply } = postcard::from_bytes(&bytes).unwrap() else {
            panic!("decoded the wrong frame");
        };
        assert_eq!(ctx, Uuid::from_u128(7));
        assert!(matches!(reply, HttpReply::Upgrade { callback } if callback.as_ref() == "on_ws"));
    }

    #[test]
    fn responses_outlive_their_buffer() {
        let response = HttpResponse::new(StatusCode::from_u16(200))
            .header("content-type", "text/plain")
            .body("hi");
        let bytes = postcard::to_allocvec(&response).unwrap();
        let decoded: HttpResponse = postcard::from_bytes(&bytes).unwrap();
        drop(bytes);

        assert_eq!(decoded.headers[0].value, "text/plain");
        assert_eq!(decoded.body, b"hi");
    }

    #[test]
    fn requests_borrow_from_their_buffer() {
        let request = HttpRequest {
            gateway: Sri::NIL,
            ctx: Uuid::from_u128(7),
            method: Method::Post,
            path: "/echo",
            query: Some("a=1"),
            headers: alloc::vec![Header {
                name: Cow::Borrowed("Content-Type"),
                value: Cow::Borrowed("text/plain"),
            }],
            body: b"ping",
        };
        let bytes = postcard::to_allocvec(&request).unwrap();
        let decoded: HttpRequest<'_> = postcard::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.path, "/echo");
        assert_eq!(decoded.query, Some("a=1"));
        assert_eq!(decoded.header("content-type"), Some("text/plain"));
        assert_eq!(decoded.body, b"ping");
        assert!(matches!(decoded.headers[0].value, Cow::Borrowed(_)));
        assert!(bytes.as_ptr_range().contains(&decoded.body.as_ptr()));
    }
}
