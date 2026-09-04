//! Wire types for the datalayer's blob (content-addressed store + path) API.
//!
//! Mirrors the host-side `db_commons::models` blob operations, minus the
//! transaction id (the host reuses the handler transaction) and with the guest
//! [`Scope`], which the host resolves against the calling cell.
//!
//! Blobs are stored by content and reached through a path linked to them, so
//! storing a file is `blob_store` (bytes → [`BlobId`]) followed by `blob_link`
//! ([`BlobId`] → path).

use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::db::Scope;

/// A path a blob is reachable under within a scope, e.g. `/index.html`.
pub type BlobPath = String;

/// Content hash of a stored blob.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum BlobHash {
    /// A SHA-256 digest of the blob's bytes.
    Sha2([u8; 32]),
}

/// Identifies a stored blob: its content hash plus the scope holding it.
///
/// The `scope` comes back from the host already resolved (a cell's private
/// namespace arrives as [`crate::db::Namespace::Public`] naming the cell), so
/// handing a [`BlobId`] straight back to another call round-trips correctly.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct BlobId {
    pub scope: Scope,
    pub hash: BlobHash,
}

/// A byte range of a blob, for reading large blobs in pieces.
///
/// A `length` of 0 requests no data — use it to test for existence or to
/// resolve a path's [`BlobId`] and total size without transferring the blob.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct ChunkRange {
    pub offset: u64,
    pub length: u64,
}

/// The blob data a resolve returned, plus what it was resolved from.
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct BlobResponse {
    /// The returned blob bytes (the requested range of them).
    pub blob: Vec<u8>,
    pub blob_id: BlobId,
    /// The range actually returned, clamped to the blob's size.
    pub range: Option<ChunkRange>,
    /// Total size of the blob, independent of the requested range.
    pub total_len: u64,
}

/// Stores blob bytes in `scope`, returning their [`BlobId`].
///
/// The bytes are *not* part of this request: `blob_store` takes them as a
/// separate pointer/length so a cell can hand over static data (an
/// `include_bytes!` asset) without copying it onto the guest heap first.
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct BlobStoreRequest {
    pub scope: Scope,
}

/// Wire response to a [`BlobStoreRequest`].
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct BlobStoreResponse {
    pub blob_id: BlobId,
}

/// Links a stored blob to a path, making it resolvable by that path.
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct BlobLinkRequest {
    pub blob_id: BlobId,
    pub path: BlobPath,
}

/// Removes a path. The blob itself survives while other paths reference it.
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct BlobUnlinkRequest {
    pub scope: Scope,
    pub path: BlobPath,
}

/// Re-points a path, leaving the blob untouched.
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct BlobMoveRequest {
    pub scope: Scope,
    pub old_path: BlobPath,
    pub new_path: BlobPath,
}

/// Reads a blob by id.
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct BlobResolveRequest {
    pub blob_id: BlobId,
    /// The byte range to return; `None` for the whole blob.
    pub range: Option<ChunkRange>,
}

/// Reads the blob a path points at.
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct PathResolveRequest {
    pub scope: Scope,
    pub path: BlobPath,
    /// The byte range to return; `None` for the whole blob.
    pub range: Option<ChunkRange>,
}

/// Response to either resolve; `None` when nothing was found.
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct ResolveResponse {
    pub blob: Option<BlobResponse>,
}

/// Lists the paths linked within a scope.
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct PathsListRequest {
    pub scope: Scope,
    /// Maximum number of paths to return; `None` for no limit.
    pub limit: Option<usize>,
}

/// Wire response to a [`PathsListRequest`].
#[derive(Debug, Serialize, Deserialize)]
#[allow(missing_docs)]
pub struct PathsListResponse {
    pub paths: Vec<BlobPath>,
}
