//! Table datalayer host functions.

use core::ffi::c_int;

use db_client::v1::models::{
    Cursor as DbCursor, TbOrderBy as DbTbOrderBy, tb_append, tb_count, tb_delete, tb_get,
    tb_insert, tb_list,
};
use myrmic_common::db::{
    Cursor as WasmCursor, TbAppendRequest, TbCountRequest, TbCountResponse, TbDeleteRequest,
    TbGetRequest, TbGetResponse, TbInsertRequest, TbInsertResponse, TbListRequest, TbListResponse,
    TbOrderBy as WasmTbOrderBy,
};
use wasm_runtime_macros::host_function;

use crate::imports::db::{apply, decode_request, defer, encode_response, transform_scope};
use crate::tri;

fn transform_cursor(cursor: WasmCursor) -> DbCursor {
    match cursor {
        WasmCursor::After(id) => DbCursor::After(id),
        WasmCursor::At(id) => DbCursor::At(id),
        WasmCursor::Skip(n) => DbCursor::Skip(n),
    }
}

fn to_db_order(order: WasmTbOrderBy) -> DbTbOrderBy {
    match order {
        WasmTbOrderBy::KeyAsc => DbTbOrderBy::KeyAsc,
        WasmTbOrderBy::KeyDesc => DbTbOrderBy::KeyDesc,
    }
}

#[host_function]
fn tb_insert(
    req_buffer: *const u8,
    req_length: c_int,
    rsp_buffer: *mut u8,
    rsp_length: c_int,
) -> c_int {
    let TbInsertRequest {
        scope,
        table,
        eid,
        value,
    } = tri!(
        // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
        unsafe { decode_request(req_buffer, req_length, "table insert request") }
    );

    let scope = tri!(transform_scope(scope));

    let tb_insert::Response { eid } = tri!(apply(tb_insert::Op {
        scope,
        table,
        eid,
        value,
    }));

    // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
    unsafe {
        encode_response(
            rsp_buffer,
            rsp_length,
            &TbInsertResponse { eid },
            "table insert response",
        )
    }
}

/// [`tb_insert`] without the id in the reply, which is what the typed `Table`
/// surface uses — nothing reads the id back, so the write can be deferred.
#[host_function]
fn tb_append(buffer: *const u8, length: c_int) -> c_int {
    let TbAppendRequest {
        scope,
        table,
        eid,
        value,
    } = tri!(
        // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
        unsafe { decode_request(buffer, length, "table append request") }
    );

    let scope = tri!(transform_scope(scope));

    defer(tb_append::Op {
        scope,
        table,
        eid,
        value,
    })
}

#[host_function]
fn tb_count(
    req_buffer: *const u8,
    req_length: c_int,
    rsp_buffer: *mut u8,
    rsp_length: c_int,
) -> c_int {
    let TbCountRequest { scope, table } = tri!(
        // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
        unsafe { decode_request(req_buffer, req_length, "table count request") }
    );

    let scope = tri!(transform_scope(scope));

    let tb_count::Response { count } = tri!(apply(tb_count::Op { scope, table }));

    // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
    unsafe {
        encode_response(
            rsp_buffer,
            rsp_length,
            &TbCountResponse { count },
            "table count response",
        )
    }
}

#[host_function]
fn tb_get(
    req_buffer: *const u8,
    req_length: c_int,
    rsp_buffer: *mut u8,
    rsp_length: c_int,
) -> c_int {
    let TbGetRequest { scope, table, eid } = tri!(
        // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
        unsafe { decode_request(req_buffer, req_length, "table get request") }
    );

    let scope = tri!(transform_scope(scope));

    let tb_get::Response { value } = tri!(apply(tb_get::Op { scope, table, eid }));

    // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
    unsafe {
        encode_response(
            rsp_buffer,
            rsp_length,
            &TbGetResponse { value },
            "table get response",
        )
    }
}

#[host_function]
fn tb_list(
    req_buffer: *const u8,
    req_length: c_int,
    rsp_buffer: *mut u8,
    rsp_length: c_int,
) -> c_int {
    let TbListRequest {
        scope,
        table,
        cursor,
        limit,
        order,
    } = tri!(
        // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
        unsafe { decode_request(req_buffer, req_length, "table list request") }
    );

    let scope = tri!(transform_scope(scope));
    let cursor = cursor.map(transform_cursor);

    let tb_list::Response { entities } = tri!(apply(tb_list::Op {
        scope,
        table,
        cursor,
        limit,
        order: order.map(to_db_order),
    }));

    // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
    unsafe {
        encode_response(
            rsp_buffer,
            rsp_length,
            &TbListResponse { entities },
            "table list response",
        )
    }
}

#[host_function]
fn tb_delete(buffer: *const u8, length: c_int) -> c_int {
    let TbDeleteRequest { scope, table, eid } = tri!(
        // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
        unsafe { decode_request(buffer, length, "table delete request") }
    );

    let scope = tri!(transform_scope(scope));

    defer(tb_delete::Op { scope, table, eid })
}
