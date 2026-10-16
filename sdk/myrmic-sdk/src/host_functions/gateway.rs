//! Serving HTTP and `WebSocket` clients through the socket gateway.
//!
//! A cell mounts a URL prefix and names the command every request under it is
//! delivered to. The handler answers each [`HttpRequest`] once: with a
//! response, a blob from the store, or by upgrading it to a `WebSocket` whose
//! [`WsEvent`]s go to another command. Routing within the mount is the
//! handler's own logic.
//!
//! The route is the cell's resource: it lives as long as the cell does and is
//! torn down with it.
//!
//! ```ignore
//! use myrmic_sdk::gateway::{self, Asset, HttpRequest, WsEvent};
//!
//! #[myrmic_sdk::init]
//! fn init(md: Metadata) -> myrmic_sdk::Result<()> {
//!     gateway::assets(md.id).put("/index.html", include_bytes!("../dist/index.html"))?;
//!     gateway::mount("/chat", Callback::of::<on_http>())?;
//!     Ok(())
//! }
//!
//! #[myrmic_sdk::cmd]
//! fn on_http(_md: Metadata, req: HttpRequest) -> myrmic_sdk::Result<()> {
//!     match req.path.as_str() {
//!         "/ws" => req.upgrade(Callback::of::<on_ws>()),
//!         path => {
//!             let asset = Asset::new(path).index("/index.html");
//!             req.asset(asset)
//!         }
//!     }
//! }
//!
//! #[myrmic_sdk::cmd]
//! fn on_ws(_md: Metadata, event: WsEvent) -> myrmic_sdk::Result<()> {
//!     match event {
//!         WsEvent::Message(socket, message) => socket.send(message),
//!         WsEvent::Open(_) | WsEvent::Closed(_) => Ok(()),
//!     }
//! }
//! ```

use alloc::string::ToString;
use core::ops::Deref;

use myrmic_common::gateway::{
    self as wire, GATEWAY_COMMAND, GatewayFrame, HttpReply, MountRequest, UnmountRequest,
    asset_scope,
};
use myrmic_common::types::error::SUCCESS;
use serde::{Deserialize, Serialize};

use crate::db::store::BlobStore;
use crate::{Bytes, Callback, Codec, Command, Decoder, Encoder, Postcard, Result, Sri, String};

pub use myrmic_common::gateway::{
    Asset, Fallback, GatewayError, Header, HttpResponse, Method, SessionId, WsMessage,
};
pub use myrmic_common::types::web::StatusCode;

mod c_functions {
    use core::ffi::c_int;

    #[link(wasm_import_module = "gateway")]
    unsafe extern "C" {
        /// Register a route for the calling cell. The payload is a
        /// postcard-serialized `MountRequest`; the host takes the owner from
        /// the calling cell's identity, so a cell cannot mount for another.
        ///
        /// # Returns:
        /// - [`myrmic_common::types::error::SUCCESS`] on success
        /// - negative error code on failure (see `GatewayError::try_from`)
        pub(super) fn gateway_mount(buffer: *const u8, length: c_int) -> c_int;

        /// Drop a route the calling cell owns. The payload is a
        /// postcard-serialized `UnmountRequest`.
        pub(super) fn gateway_unmount(buffer: *const u8, length: c_int) -> c_int;
    }
}

/// The blob store this cell serves its gateway assets from.
///
/// Files written here are what an [`Asset`] reply with no explicit scope
/// serves; the scope is per-cell, so its contents go away with the cell. `sri`
/// is the cell's own id, from `Metadata::id`.
#[must_use]
pub fn assets(sri: Sri) -> BlobStore {
    BlobStore::new(asset_scope(&sri.to_string()))
}

/// Serves `mount`, e.g. `/chat`, delivering every request under it to `http`.
///
/// Replaces this cell's existing route at the same mount; fails with
/// [`GatewayError::MountTaken`] if another cell owns it.
pub fn mount(
    mount: impl Into<String>,
    http: Callback<HttpRequest>,
) -> core::result::Result<(), GatewayError> {
    let request = MountRequest {
        mount: mount.into(),
        http: http.into(),
    };
    let payload = postcard::to_allocvec(&request).map_err(|_| GatewayError::WriteFailed)?;

    // SAFETY: calling the imported function with pointer and length of guest linear memory.
    let status =
        unsafe { c_functions::gateway_mount(payload.as_ptr(), payload.len() as core::ffi::c_int) };
    to_result(status)
}

/// Drops the route this cell owns at `mount`.
///
/// Rarely needed — a cell's routes are removed when it is undeployed.
pub fn unmount(mount: impl Into<String>) -> core::result::Result<(), GatewayError> {
    let request = UnmountRequest {
        mount: mount.into(),
    };
    let payload = postcard::to_allocvec(&request).map_err(|_| GatewayError::WriteFailed)?;

    // SAFETY: calling the imported function with pointer and length of guest linear memory.
    let status = unsafe {
        c_functions::gateway_unmount(payload.as_ptr(), payload.len() as core::ffi::c_int)
    };
    to_result(status)
}

/// An HTTP request under one of this cell's mounts.
///
/// Answer it exactly once, with [`respond`](Self::respond),
/// [`asset`](Self::asset) or [`upgrade`](Self::upgrade). An unanswered request
/// times out at the gateway. A request is a plain message, so it can be sent to
/// another cell, which answers it in turn.
#[derive(Debug, Clone)]
pub struct HttpRequest(wire::HttpRequest);

impl HttpRequest {
    /// Sends `response` to the client.
    pub fn respond(self, response: HttpResponse) -> Result<()> {
        self.reply(HttpReply::Response(response))
    }

    /// Serves a blob. An [`Asset`] without a scope reads from the asset scope
    /// of the cell that calls this.
    pub fn asset(self, asset: Asset) -> Result<()> {
        self.reply(HttpReply::Asset(asset))
    }

    /// Accepts the `WebSocket` upgrade; the socket's events go to `callback`
    /// on the cell that calls this. A request that didn't ask for an upgrade
    /// is answered with `426 Upgrade Required`.
    pub fn upgrade(self, callback: Callback<WsEvent>) -> Result<()> {
        self.reply(HttpReply::Upgrade {
            callback: callback.into(),
        })
    }

    /// Unwraps the wire request.
    #[must_use]
    pub fn into_inner(self) -> wire::HttpRequest {
        self.0
    }

    fn reply(self, reply: HttpReply) -> Result<()> {
        send_frame(
            self.0.gateway,
            &GatewayFrame::Reply {
                ctx: self.0.ctx,
                reply,
            },
        )
    }
}

impl Deref for HttpRequest {
    type Target = wire::HttpRequest;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Decoder for HttpRequest {
    fn from_bytes(bytes: Bytes) -> Result<Self> {
        <Postcard as Codec>::decode(&bytes).map(Self)
    }
}

impl Encoder for HttpRequest {
    fn to_bytes(&self) -> Result<Bytes> {
        <Postcard as Codec>::encode(&self.0)
    }
}

/// An upgraded `WebSocket`. Cheap to copy and serializable, so it can be kept
/// (in memory or the db) and sent to later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Socket(wire::Socket);

impl Socket {
    /// The gateway holding the connection.
    #[must_use]
    pub fn gateway(&self) -> Sri {
        self.0.gateway
    }

    /// The id of the request that was upgraded.
    #[must_use]
    pub fn ctx(&self) -> SessionId {
        self.0.ctx
    }

    /// Sends a frame to the client. Dropped if the socket has closed.
    pub fn send(&self, message: WsMessage) -> Result<()> {
        send_frame(
            self.0.gateway,
            &GatewayFrame::Send {
                ctx: self.0.ctx,
                message,
            },
        )
    }

    /// Closes the socket. A [`WsEvent::Closed`] follows.
    pub fn close(self) -> Result<()> {
        send_frame(self.0.gateway, &GatewayFrame::Close { ctx: self.0.ctx })
    }
}

/// What happened on a [`Socket`].
#[derive(Debug, Clone)]
pub enum WsEvent {
    /// The upgrade completed; the socket can be sent to.
    Open(Socket),
    /// The client sent a frame.
    Message(Socket, WsMessage),
    /// The socket is gone. Nothing more arrives on it, and sends are dropped.
    Closed(Socket),
}

impl Decoder for WsEvent {
    fn from_bytes(bytes: Bytes) -> Result<Self> {
        Ok(match <Postcard as Codec>::decode(&bytes)? {
            wire::WsEvent::Open(socket) => Self::Open(Socket(socket)),
            wire::WsEvent::Message(socket, message) => Self::Message(Socket(socket), message),
            wire::WsEvent::Closed(socket) => Self::Closed(Socket(socket)),
        })
    }
}

impl Encoder for WsEvent {
    fn to_bytes(&self) -> Result<Bytes> {
        let event = match self {
            Self::Open(socket) => wire::WsEvent::Open(socket.0),
            Self::Message(socket, message) => wire::WsEvent::Message(socket.0, message.clone()),
            Self::Closed(socket) => wire::WsEvent::Closed(socket.0),
        };
        <Postcard as Codec>::encode(&event)
    }
}

struct Frame<'a>(&'a GatewayFrame);

impl Encoder for Frame<'_> {
    fn to_bytes(&self) -> Result<Bytes> {
        <Postcard as Codec>::encode(self.0)
    }
}

fn send_frame(gateway: Sri, frame: &GatewayFrame) -> Result<()> {
    let command = Command::try_from(GATEWAY_COMMAND)?;
    crate::send(gateway, command, &Frame(frame))
}

fn to_result(status: core::ffi::c_int) -> core::result::Result<(), GatewayError> {
    match status {
        code if code == SUCCESS => Ok(()),
        code => Err(GatewayError::try_from(code)
            .unwrap_or_else(|code| panic!("unexpected gateway error code from host: {code}"))),
    }
}
