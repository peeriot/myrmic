//! Host functions used to interact with the datalayer using its blob API.
//!
//! Blobs are content-addressed: [`blob_store`] takes bytes and returns a
//! [`BlobId`], [`blob_link`] makes that blob reachable under a path, and
//! [`path_resolve`] reads it back. Paths within a scope are what the gateway
//! serves static assets from.

use alloc::string::String;
use alloc::vec::Vec;
use myrmic_common::db::{
    BlobId, BlobLinkRequest, BlobMoveRequest, BlobPath, BlobResolveRequest, BlobResponse,
    BlobStoreRequest, BlobStoreResponse, BlobUnlinkRequest, ChunkRange, PathResolveRequest,
    PathsListRequest, PathsListResponse, ResolveResponse, Scope,
};

use crate::error::ErrorCode;
use crate::{ApiError, ApiResult};

mod c_functions {
    use core::ffi::c_int;

    #[link(wasm_import_module = "db")]
    unsafe extern "C" {
        /// Store blob bytes, returning their `BlobId`.
        ///
        /// The bytes travel outside the request so that static data (an
        /// `include_bytes!` asset) can be handed to the host directly, without
        /// being copied into a serialized request on the guest heap first.
        ///
        /// # Arguments:
        /// - `buffer_req`: pointer to the serialized `BlobStoreRequest`
        /// - `len_req`: length of that buffer
        /// - `buffer_blob`: pointer to the blob bytes
        /// - `len_blob`: number of blob bytes
        /// - `buffer_rsp`: where the host writes the `BlobStoreResponse`
        /// - `len_rsp`: capacity of the response buffer
        pub(super) fn blob_store(
            buffer_req: *const u8,
            len_req: c_int,
            buffer_blob: *const u8,
            len_blob: c_int,
            buffer_rsp: *mut u8,
            len_rsp: c_int,
        ) -> c_int;

        /// Link a stored blob to a path so it resolves by that path.
        pub(super) fn blob_link(buffer_req: *const u8, len_req: c_int) -> c_int;

        /// Remove a path. The blob survives while other paths reference it.
        pub(super) fn blob_unlink(buffer_req: *const u8, len_req: c_int) -> c_int;

        /// Re-point a path at a new location, leaving the blob untouched.
        pub(super) fn blob_move(buffer_req: *const u8, len_req: c_int) -> c_int;

        /// Read a blob by id, optionally a range of it.
        pub(super) fn blob_resolve(
            buffer_req: *const u8,
            len_req: c_int,
            buffer_rsp: *mut u8,
            len_rsp: c_int,
        ) -> c_int;

        /// Read the blob a path points at, optionally a range of it.
        pub(super) fn path_resolve(
            buffer_req: *const u8,
            len_req: c_int,
            buffer_rsp: *mut u8,
            len_rsp: c_int,
        ) -> c_int;

        /// List the paths linked within a scope.
        pub(super) fn paths_list(
            buffer_req: *const u8,
            len_req: c_int,
            buffer_rsp: *mut u8,
            len_rsp: c_int,
        ) -> c_int;
    }
}

fn serialize<T: serde::Serialize>(
    request: &T,
    buffer: &mut [u8],
    context: &'static str,
) -> ApiResult<usize> {
    Ok(postcard::to_slice(request, buffer)
        .map_err(|_e| ApiError::Serde(context))?
        .len())
}

/// Stores `blob` in `scope` and returns its content id.
pub fn blob_store(
    scope: Scope,
    blob: &[u8],
    request_buffer: &mut [u8],
    response_buffer: &mut [u8],
) -> ApiResult<BlobId> {
    let request = BlobStoreRequest { scope };
    let len = serialize(&request, request_buffer, "serializing blob store request")?;

    // SAFETY: calling the imported function with pointers and lengths of guest
    // linear memory; `blob` is borrowed for the duration of the call.
    unsafe {
        c_functions::blob_store(
            request_buffer.as_ptr(),
            len as i32,
            blob.as_ptr(),
            blob.len() as i32,
            response_buffer.as_mut_ptr(),
            response_buffer.len() as i32,
        )
    }
    .to_result()?;

    let response = postcard::from_bytes::<BlobStoreResponse>(response_buffer)
        .map_err(|_e| ApiError::Serde("deserializing blob store response"))?;
    Ok(response.blob_id)
}

/// Makes `blob_id` reachable under `path`.
pub fn blob_link(blob_id: BlobId, path: BlobPath, buffer: &mut [u8]) -> ApiResult<()> {
    let request = BlobLinkRequest { blob_id, path };
    let len = serialize(&request, buffer, "serializing blob link request")?;

    // SAFETY: calling the imported function with pointer and length of guest linear memory.
    unsafe { c_functions::blob_link(buffer.as_ptr(), len as i32) }.to_result()
}

/// Removes `path` from `scope`.
pub fn blob_unlink(scope: Scope, path: BlobPath, buffer: &mut [u8]) -> ApiResult<()> {
    let request = BlobUnlinkRequest { scope, path };
    let len = serialize(&request, buffer, "serializing blob unlink request")?;

    // SAFETY: calling the imported function with pointer and length of guest linear memory.
    unsafe { c_functions::blob_unlink(buffer.as_ptr(), len as i32) }.to_result()
}

/// Moves the link at `old_path` to `new_path`.
pub fn blob_move(
    scope: Scope,
    old_path: BlobPath,
    new_path: BlobPath,
    buffer: &mut [u8],
) -> ApiResult<()> {
    let request = BlobMoveRequest {
        scope,
        old_path,
        new_path,
    };
    let len = serialize(&request, buffer, "serializing blob move request")?;

    // SAFETY: calling the imported function with pointer and length of guest linear memory.
    unsafe { c_functions::blob_move(buffer.as_ptr(), len as i32) }.to_result()
}

/// Reads the blob with `blob_id`, or `range` of it.
pub fn blob_resolve(
    blob_id: BlobId,
    range: Option<ChunkRange>,
    request_buffer: &mut [u8],
    response_buffer: &mut [u8],
) -> ApiResult<Option<BlobResponse>> {
    let request = BlobResolveRequest { blob_id, range };
    let len = serialize(&request, request_buffer, "serializing blob resolve request")?;

    // SAFETY: calling the imported function with pointers and lengths of guest linear memory.
    unsafe {
        c_functions::blob_resolve(
            request_buffer.as_ptr(),
            len as i32,
            response_buffer.as_mut_ptr(),
            response_buffer.len() as i32,
        )
    }
    .to_result()?;

    let response = postcard::from_bytes::<ResolveResponse>(response_buffer)
        .map_err(|_e| ApiError::Serde("deserializing blob resolve response"))?;
    Ok(response.blob)
}

/// Reads the blob linked at `path` in `scope`, or `range` of it.
pub fn path_resolve(
    scope: Scope,
    path: BlobPath,
    range: Option<ChunkRange>,
    request_buffer: &mut [u8],
    response_buffer: &mut [u8],
) -> ApiResult<Option<BlobResponse>> {
    let request = PathResolveRequest { scope, path, range };
    let len = serialize(&request, request_buffer, "serializing path resolve request")?;

    // SAFETY: calling the imported function with pointers and lengths of guest linear memory.
    unsafe {
        c_functions::path_resolve(
            request_buffer.as_ptr(),
            len as i32,
            response_buffer.as_mut_ptr(),
            response_buffer.len() as i32,
        )
    }
    .to_result()?;

    let response = postcard::from_bytes::<ResolveResponse>(response_buffer)
        .map_err(|_e| ApiError::Serde("deserializing path resolve response"))?;
    Ok(response.blob)
}

/// Lists the paths linked within `scope`.
pub fn paths_list(
    scope: Scope,
    limit: Option<usize>,
    request_buffer: &mut [u8],
    response_buffer: &mut [u8],
) -> ApiResult<Vec<String>> {
    let request = PathsListRequest { scope, limit };
    let len = serialize(&request, request_buffer, "serializing paths list request")?;

    // SAFETY: calling the imported function with pointers and lengths of guest linear memory.
    unsafe {
        c_functions::paths_list(
            request_buffer.as_ptr(),
            len as i32,
            response_buffer.as_mut_ptr(),
            response_buffer.len() as i32,
        )
    }
    .to_result()?;

    let response = postcard::from_bytes::<PathsListResponse>(response_buffer)
        .map_err(|_e| ApiError::Serde("deserializing paths list response"))?;
    Ok(response.paths)
}
