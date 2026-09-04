//! Declaring how the socket gateway should serve this cell.
//!
//! A cell asks to be served at a URL prefix and the host records the route
//! against the cell's own identity. The route is the cell's resource: it lives
//! as long as the cell does and is torn down with it, so nothing outside the
//! cell has to describe or clean up its front end.
//!
//! ```ignore
//! #[myrmic_sdk::init]
//! fn init(md: Metadata) -> myrmic_sdk::Result<()> {
//!     let assets = myrmic_sdk::gateway::assets(md.id);
//!     assets.put("/index.html", include_bytes!("../dist/index.html"))?;
//!
//!     myrmic_sdk::gateway::mount("/chat")
//!         .api("/api")
//!         .ws("/ws")
//!         .index("/index.html")
//!         .bind()?;
//!     Ok(())
//! }
//! ```

use alloc::string::ToString;
use myrmic_common::db::Scope;
use myrmic_common::gateway::{AssetMount, MountRequest, UnmountRequest, asset_scope};
use myrmic_common::types::error::SUCCESS;

use crate::db::store::BlobStore;
use crate::{Sri, String};

pub use myrmic_common::gateway::{Fallback, GatewayError};

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
/// Write files here and mount with [`Mount::index`] (or [`Mount::assets`]) to
/// have them served; the scope is per-cell, so its contents go away with the
/// cell. `sri` is the cell's own id, from `Metadata::id`.
#[must_use]
pub fn assets(sri: Sri) -> BlobStore {
    BlobStore::new(asset_scope(&sri.to_string()))
}

/// Starts describing a route served at `mount`, e.g. `/chat`.
///
/// Nothing is registered until [`Mount::bind`] is called.
pub fn mount(mount: impl Into<String>) -> Mount {
    Mount {
        request: MountRequest {
            mount: mount.into(),
            api: None,
            ws: None,
            assets: None,
        },
    }
}

/// Drops the route this cell owns at `mount`.
///
/// Rarely needed — a cell's routes are removed when it is undeployed.
pub fn unmount(mount: impl Into<String>) -> Result<(), GatewayError> {
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

/// A route being described. See [`mount`].
pub struct Mount {
    request: MountRequest,
}

impl Mount {
    /// Serves the cell command/event API at `path`, relative to the mount.
    ///
    /// `GET` opens the receive stream, `POST` sends one message. Messages that
    /// name no target are addressed to this cell.
    #[must_use]
    pub fn api(mut self, path: impl Into<String>) -> Self {
        self.request.api = Some(path.into());
        self
    }

    /// Serves the `WebSocket` upgrade at `path`, relative to the mount.
    #[must_use]
    pub fn ws(mut self, path: impl Into<String>) -> Self {
        self.request.ws = Some(path.into());
        self
    }

    /// Serves static assets from this cell's [`assets`] store.
    #[must_use]
    pub fn assets(mut self) -> Self {
        self.asset_mount();
        self
    }

    /// Serves `path` at the mount root, and as the single-page-app fallback.
    /// Implies [`Mount::assets`].
    #[must_use]
    pub fn index(mut self, path: impl Into<String>) -> Self {
        self.asset_mount().index = Some(path.into());
        self
    }

    /// What to serve when an asset path matches nothing. Implies
    /// [`Mount::assets`]. Defaults to [`Fallback::Spa`].
    #[must_use]
    pub fn fallback(mut self, fallback: Fallback) -> Self {
        self.asset_mount().fallback = fallback;
        self
    }

    /// Serves assets from an explicit scope rather than this cell's own.
    /// Implies [`Mount::assets`].
    #[must_use]
    pub fn scope(mut self, scope: Scope) -> Self {
        self.asset_mount().scope = Some(scope);
        self
    }

    /// Registers the route. Replaces this cell's existing route at the same
    /// mount; fails with [`GatewayError::MountTaken`] if another cell owns it.
    pub fn bind(self) -> Result<(), GatewayError> {
        let payload =
            postcard::to_allocvec(&self.request).map_err(|_| GatewayError::WriteFailed)?;

        // SAFETY: calling the imported function with pointer and length of guest linear memory.
        let status = unsafe {
            c_functions::gateway_mount(payload.as_ptr(), payload.len() as core::ffi::c_int)
        };
        to_result(status)
    }

    fn asset_mount(&mut self) -> &mut AssetMount {
        self.request.assets.get_or_insert_with(AssetMount::default)
    }
}

fn to_result(status: core::ffi::c_int) -> Result<(), GatewayError> {
    match status {
        code if code == SUCCESS => Ok(()),
        code => Err(GatewayError::try_from(code)
            .unwrap_or_else(|code| panic!("unexpected gateway error code from host: {code}"))),
    }
}
