//! Host side of the datalayer's blob API.
//!
//! Blobs are content-addressed and reached through paths linked to them. The
//! gateway serves a cell's static assets straight out of these paths, so this
//! is how a cell publishes its own front end.

use cell_protocol::Sri;
use db_client::v1::models::{
    BlobHash as DbBlobHash, BlobId as DbBlobId, blob_link, blob_move, blob_resolve, blob_store,
    blob_unlink, path_resolve, paths_list,
};
use myrmic_common::db::{
    BlobHash, BlobId, BlobLinkRequest, BlobMoveRequest, BlobResolveRequest, BlobResponse,
    BlobStoreRequest, BlobStoreResponse, BlobUnlinkRequest, ChunkRange, PathResolveRequest,
    PathsListRequest, PathsListResponse, ResolveResponse,
};
use myrmic_common::types::error::SUCCESS;
use wasmtime::Caller;

use crate::wasm::{
    cell::state::CellState,
    host_functions::{
        as_slice,
        db::{apply, defer, transform_scope, untransform_scope},
        decode, encode, tri,
    },
};

/// Converts a guest blob id into the host's, resolving its scope against the
/// calling cell exactly as any other scope is resolved.
fn transform_blob_id(caller: &mut Caller<'_, CellState>, blob_id: BlobId) -> Result<DbBlobId, i32> {
    let BlobHash::Sha2(hash) = blob_id.hash;
    Ok(DbBlobId {
        scope: transform_scope(caller, blob_id.scope)?,
        hash: DbBlobHash::Sha2(hash),
    })
}

/// Converts a host blob id back for the guest, so handing the id straight back
/// to another call round-trips.
fn untransform_blob_id(sri: &Sri, blob_id: DbBlobId) -> BlobId {
    let DbBlobHash::Sha2(hash) = blob_id.hash;
    BlobId {
        scope: untransform_scope(sri, blob_id.scope),
        hash: BlobHash::Sha2(hash),
    }
}

fn untransform_blob(sri: &Sri, response: db_client::v1::models::BlobResponse) -> BlobResponse {
    BlobResponse {
        blob: response.blob,
        blob_id: untransform_blob_id(sri, response.blob_id),
        range: response.range.map(|range| ChunkRange {
            offset: range.offset,
            length: range.length,
        }),
        total_len: response.total_len,
    }
}

fn transform_range(range: Option<ChunkRange>) -> Option<db_client::v1::models::ChunkRange> {
    range.map(|range| db_client::v1::models::ChunkRange {
        offset: range.offset,
        length: range.length,
    })
}

pub(crate) async fn blob_store(
    mut caller: Caller<'_, CellState>,
    req_ptr: u32,
    req_len: u32,
    blob_ptr: u32,
    blob_len: u32,
    rsp_ptr: u32,
    rsp_len: u32,
) -> i32 {
    let BlobStoreRequest { scope } =
        tri!(decode(&mut caller, req_ptr, req_len, "blob store request"));

    // The bytes arrive outside the request so the guest can hand over static
    // data without copying it onto its heap first.
    let blob = as_slice(&mut caller, blob_ptr as usize, blob_len as usize).to_vec();

    let scope = tri!(transform_scope(&mut caller, scope));

    let stored = tri!(apply(&mut caller, blob_store::Op { scope, blob }).await);

    let response = BlobStoreResponse {
        blob_id: untransform_blob_id(caller.data().sri(), stored.blob_id),
    };

    let () = tri!(encode(
        &mut caller,
        rsp_ptr,
        rsp_len,
        &response,
        "blob store response"
    ));

    SUCCESS
}

pub(crate) async fn blob_link(
    mut caller: Caller<'_, CellState>,
    req_ptr: u32,
    req_len: u32,
) -> i32 {
    let BlobLinkRequest { blob_id, path } =
        tri!(decode(&mut caller, req_ptr, req_len, "blob link request"));

    let blob_id = tri!(transform_blob_id(&mut caller, blob_id));

    defer(&mut caller, blob_link::Op { blob_id, path })
}

pub(crate) async fn blob_unlink(
    mut caller: Caller<'_, CellState>,
    req_ptr: u32,
    req_len: u32,
) -> i32 {
    let BlobUnlinkRequest { scope, path } =
        tri!(decode(&mut caller, req_ptr, req_len, "blob unlink request"));

    let scope = tri!(transform_scope(&mut caller, scope));

    defer(&mut caller, blob_unlink::Op { scope, path })
}

pub(crate) async fn blob_move(
    mut caller: Caller<'_, CellState>,
    req_ptr: u32,
    req_len: u32,
) -> i32 {
    let BlobMoveRequest {
        scope,
        old_path,
        new_path,
    } = tri!(decode(&mut caller, req_ptr, req_len, "blob move request"));

    let scope = tri!(transform_scope(&mut caller, scope));

    defer(
        &mut caller,
        blob_move::Op {
            scope,
            old_path,
            new_path,
        },
    )
}

pub(crate) async fn blob_resolve(
    mut caller: Caller<'_, CellState>,
    req_ptr: u32,
    req_len: u32,
    rsp_ptr: u32,
    rsp_len: u32,
) -> i32 {
    let BlobResolveRequest { blob_id, range } = tri!(decode(
        &mut caller,
        req_ptr,
        req_len,
        "blob resolve request"
    ));

    let blob_id = tri!(transform_blob_id(&mut caller, blob_id));

    let resolved = tri!(
        apply(
            &mut caller,
            blob_resolve::Op {
                blob_id,
                range: transform_range(range),
            },
        )
        .await
    );

    let response = ResolveResponse {
        blob: resolved
            .blob
            .map(|blob| untransform_blob(caller.data().sri(), blob)),
    };

    let () = tri!(encode(
        &mut caller,
        rsp_ptr,
        rsp_len,
        &response,
        "blob resolve response"
    ));

    SUCCESS
}

pub(crate) async fn path_resolve(
    mut caller: Caller<'_, CellState>,
    req_ptr: u32,
    req_len: u32,
    rsp_ptr: u32,
    rsp_len: u32,
) -> i32 {
    let PathResolveRequest { scope, path, range } = tri!(decode(
        &mut caller,
        req_ptr,
        req_len,
        "path resolve request"
    ));

    let scope = tri!(transform_scope(&mut caller, scope));

    let resolved = tri!(
        apply(
            &mut caller,
            path_resolve::Op {
                scope,
                path,
                range: transform_range(range),
            },
        )
        .await
    );

    let response = ResolveResponse {
        blob: resolved
            .blob
            .map(|blob| untransform_blob(caller.data().sri(), blob)),
    };

    let () = tri!(encode(
        &mut caller,
        rsp_ptr,
        rsp_len,
        &response,
        "path resolve response"
    ));

    SUCCESS
}

pub(crate) async fn paths_list(
    mut caller: Caller<'_, CellState>,
    req_ptr: u32,
    req_len: u32,
    rsp_ptr: u32,
    rsp_len: u32,
) -> i32 {
    let PathsListRequest { scope, limit } =
        tri!(decode(&mut caller, req_ptr, req_len, "paths list request"));

    let scope = tri!(transform_scope(&mut caller, scope));

    let listed = tri!(apply(&mut caller, paths_list::Op { scope, limit }).await);

    let () = tri!(encode(
        &mut caller,
        rsp_ptr,
        rsp_len,
        &PathsListResponse {
            paths: listed.paths
        },
        "paths list response"
    ));

    SUCCESS
}
