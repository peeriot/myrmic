//! Wire types for the gateway routing resource a cell declares for itself.
//!
//! A cell asks the host to serve it at a URL prefix; the host records the route
//! against the *calling* cell's SRI, so the route's lifetime is the cell's — it
//! is torn down when the cell is undeployed, and gateways drop routes whose
//! owner has left the cell registry.

use alloc::string::String;
use serde::{Deserialize, Serialize};

use crate::db::Scope;

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

/// Static assets to serve under the mount, read from the blob store.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AssetMount {
    /// Blob scope holding the assets. `None` uses the cell's own asset scope —
    /// the same one [`asset_scope`] names.
    pub scope: Option<Scope>,
    /// Document served at the mount root, and as the [`Fallback::Spa`] target.
    pub index: Option<String>,
    /// What to serve when no asset matches the request path.
    pub fallback: Fallback,
}

/// A cell's request to be served at `mount`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountRequest {
    /// URL prefix to serve under, e.g. `/chat`. Gateways match the longest
    /// registered prefix, so mounts may nest.
    pub mount: String,
    /// Path relative to `mount` serving the cell command/event API over HTTP:
    /// `GET` opens the event stream, `POST` sends one message. `None` disables it.
    pub api: Option<String>,
    /// Path relative to `mount` for the `WebSocket` upgrade. `None` disables it.
    pub ws: Option<String>,
    /// Static assets, or `None` for an API-only mount.
    pub assets: Option<AssetMount>,
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

/// The gateway's namespace: the routing table every gateway serves from, and
/// the static assets cells publish through it.
///
/// Replicated by every node, so any gateway can serve any application.
pub const NAMESPACE_GATEWAY: &str = "gw";

/// Database component of the scope a cell's gateway assets live in.
pub const ASSETS_DB: &str = "gateway-assets";

/// The blob scope a cell serves its own gateway assets from, isolated per cell
/// by the schema component. Write assets here; a [`MountRequest`] with no
/// explicit [`AssetMount::scope`] reads from the very same scope.
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
