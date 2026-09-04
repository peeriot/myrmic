//! Blob datalayer host functions.
//!
//! Blobs are content-addressed and reached through paths linked to them, which
//! is how a cell publishes files (such as the assets a gateway serves for it).

use alloc::vec::Vec;
use core::ffi::c_int;

use db_client::v1::models::{
    BlobHash as DbBlobHash, BlobId as DbBlobId, BlobResponse as DbBlobResponse,
    ChunkRange as DbChunkRange, blob_link, blob_move, blob_resolve, blob_store, blob_unlink,
    path_resolve, paths_list,
};
use myrmic_common::db::{
    BlobHash, BlobId, BlobLinkRequest, BlobMoveRequest, BlobResolveRequest, BlobResponse,
    BlobStoreRequest, BlobStoreResponse, BlobUnlinkRequest, ChunkRange, PathResolveRequest,
    PathsListRequest, PathsListResponse, ResolveResponse,
};
use myrmic_common::types::error::GENERIC_ERROR;
use wasm_runtime_macros::host_function;

use crate::imports::db::{
    apply, decode_request, defer, encode_response, transform_scope, untransform_scope,
};
use crate::tri;

fn transform_blob_id(blob_id: BlobId) -> Result<DbBlobId, c_int> {
    let BlobHash::Sha2(hash) = blob_id.hash;
    Ok(DbBlobId {
        scope: transform_scope(blob_id.scope)?,
        hash: DbBlobHash::Sha2(hash),
    })
}

/// Hands a blob id back to the guest in the same shape it would pass in, so an
/// id returned by [`blob_store`] can be fed straight to [`blob_link`].
fn untransform_blob_id(blob_id: DbBlobId) -> BlobId {
    let DbBlobHash::Sha2(hash) = blob_id.hash;
    BlobId {
        scope: untransform_scope(blob_id.scope),
        hash: BlobHash::Sha2(hash),
    }
}

fn untransform_blob(response: DbBlobResponse) -> BlobResponse {
    BlobResponse {
        blob: response.blob,
        blob_id: untransform_blob_id(response.blob_id),
        range: response.range.map(|range| ChunkRange {
            offset: range.offset,
            length: range.length,
        }),
        total_len: response.total_len,
    }
}

fn transform_range(range: Option<ChunkRange>) -> Option<DbChunkRange> {
    range.map(|range| DbChunkRange {
        offset: range.offset,
        length: range.length,
    })
}

/// Copies raw bytes out of guest linear memory.
///
/// # Safety
/// `buffer` must point to `length` valid bytes within the calling module's memory.
unsafe fn read_bytes(buffer: *const u8, length: c_int) -> Result<Vec<u8>, c_int> {
    if buffer.is_null() {
        log::error!("[db] blob buffer pointer is null");
        return Err(GENERIC_ERROR);
    }

    #[expect(
        clippy::cast_sign_loss,
        reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
    )]
    // safety: caller guarantees `buffer` points to `length` readable bytes
    let data = unsafe { core::slice::from_raw_parts(buffer, length as usize) };

    Ok(data.to_vec())
}

#[host_function]
fn blob_store(
    req_buffer: *const u8,
    req_length: c_int,
    blob_buffer: *const u8,
    blob_length: c_int,
    rsp_buffer: *mut u8,
    rsp_length: c_int,
) -> c_int {
    let BlobStoreRequest { scope } = tri!(
        // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
        unsafe { decode_request(req_buffer, req_length, "blob store request") }
    );

    // The bytes travel outside the request so a cell can hand over static data
    // without copying it onto its own heap first.
    let blob = tri!(
        // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
        unsafe { read_bytes(blob_buffer, blob_length) }
    );

    let scope = tri!(transform_scope(scope));

    let blob_store::Response { blob_id } = tri!(apply(blob_store::Op { scope, blob }));

    // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
    unsafe {
        encode_response(
            rsp_buffer,
            rsp_length,
            &BlobStoreResponse {
                blob_id: untransform_blob_id(blob_id),
            },
            "blob store response",
        )
    }
}

#[host_function]
fn blob_link(buffer: *const u8, length: c_int) -> c_int {
    let BlobLinkRequest { blob_id, path } = tri!(
        // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
        unsafe { decode_request(buffer, length, "blob link request") }
    );

    let blob_id = tri!(transform_blob_id(blob_id));

    defer(blob_link::Op { blob_id, path })
}

#[host_function]
fn blob_unlink(buffer: *const u8, length: c_int) -> c_int {
    let BlobUnlinkRequest { scope, path } = tri!(
        // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
        unsafe { decode_request(buffer, length, "blob unlink request") }
    );

    let scope = tri!(transform_scope(scope));

    defer(blob_unlink::Op { scope, path })
}

#[host_function]
fn blob_move(buffer: *const u8, length: c_int) -> c_int {
    let BlobMoveRequest {
        scope,
        old_path,
        new_path,
    } = tri!(
        // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
        unsafe { decode_request(buffer, length, "blob move request") }
    );

    let scope = tri!(transform_scope(scope));

    defer(blob_move::Op {
        scope,
        old_path,
        new_path,
    })
}

#[host_function]
fn blob_resolve(
    req_buffer: *const u8,
    req_length: c_int,
    rsp_buffer: *mut u8,
    rsp_length: c_int,
) -> c_int {
    let BlobResolveRequest { blob_id, range } = tri!(
        // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
        unsafe { decode_request(req_buffer, req_length, "blob resolve request") }
    );

    let blob_id = tri!(transform_blob_id(blob_id));

    let blob_resolve::Response { blob } = tri!(apply(blob_resolve::Op {
        blob_id,
        range: transform_range(range),
    }));

    // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
    unsafe {
        encode_response(
            rsp_buffer,
            rsp_length,
            &ResolveResponse {
                blob: blob.map(untransform_blob),
            },
            "blob resolve response",
        )
    }
}

#[host_function]
fn path_resolve(
    req_buffer: *const u8,
    req_length: c_int,
    rsp_buffer: *mut u8,
    rsp_length: c_int,
) -> c_int {
    let PathResolveRequest { scope, path, range } = tri!(
        // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
        unsafe { decode_request(req_buffer, req_length, "path resolve request") }
    );

    let scope = tri!(transform_scope(scope));

    let path_resolve::Response { blob } = tri!(apply(path_resolve::Op {
        scope,
        path,
        range: transform_range(range),
    }));

    // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
    unsafe {
        encode_response(
            rsp_buffer,
            rsp_length,
            &ResolveResponse {
                blob: blob.map(untransform_blob),
            },
            "path resolve response",
        )
    }
}

#[host_function]
fn paths_list(
    req_buffer: *const u8,
    req_length: c_int,
    rsp_buffer: *mut u8,
    rsp_length: c_int,
) -> c_int {
    let PathsListRequest { scope, limit } = tri!(
        // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
        unsafe { decode_request(req_buffer, req_length, "paths list request") }
    );

    let scope = tri!(transform_scope(scope));

    let paths_list::Response { paths } = tri!(apply(paths_list::Op { scope, limit }));

    // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
    unsafe {
        encode_response(
            rsp_buffer,
            rsp_length,
            &PathsListResponse { paths },
            "paths list response",
        )
    }
}
