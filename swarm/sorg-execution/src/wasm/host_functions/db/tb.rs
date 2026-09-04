use db_client::v1::models::{
    Cursor as DbCursor, TbOrderBy as DbTbOrderBy, tb_append, tb_count, tb_delete, tb_get,
    tb_insert, tb_list,
};
use myrmic_common::db::{
    Cursor as WasmCursor, TbAppendRequest, TbCountRequest, TbCountResponse, TbDeleteRequest,
    TbGetRequest, TbGetResponse, TbInsertRequest, TbInsertResponse, TbListRequest, TbListResponse,
    TbOrderBy as WasmTbOrderBy,
};
use myrmic_common::types::error::SUCCESS;
use wasmtime::Caller;

use crate::wasm::{
    cell::state::CellState,
    host_functions::{
        db::{apply, defer, transform_scope},
        decode, encode, tri,
    },
};

fn transform_cursor(cursor: WasmCursor) -> DbCursor {
    match cursor {
        WasmCursor::After(id) => DbCursor::After(id),
        WasmCursor::At(id) => DbCursor::At(id),
        WasmCursor::Skip(n) => DbCursor::Skip(n),
    }
}

fn transform_order(order: WasmTbOrderBy) -> DbTbOrderBy {
    match order {
        WasmTbOrderBy::KeyAsc => DbTbOrderBy::KeyAsc,
        WasmTbOrderBy::KeyDesc => DbTbOrderBy::KeyDesc,
    }
}

pub(crate) async fn tb_insert(
    mut caller: Caller<'_, CellState>,
    req_ptr: u32,
    req_len: u32,
    rsp_ptr: u32,
    rsp_len: u32,
) -> i32 {
    let TbInsertRequest {
        scope,
        table,
        eid,
        value,
    } = tri!(decode(
        &mut caller,
        req_ptr,
        req_len,
        "table insert request"
    ));

    let scope = tri!(transform_scope(&mut caller, scope));

    let inserted = tri!(
        apply(
            &mut caller,
            tb_insert::Op {
                scope,
                table,
                eid,
                value,
            },
        )
        .await
    );
    let response = TbInsertResponse { eid: inserted.eid };

    let () = tri!(encode(
        &mut caller,
        rsp_ptr,
        rsp_len,
        &response,
        "table insert response"
    ));

    SUCCESS
}

/// [`tb_insert()`] without an id in the reply, so the write is buffered into the
/// handler's application instead of costing a round trip.
pub(crate) async fn tb_append(
    mut caller: Caller<'_, CellState>,
    req_ptr: u32,
    req_len: u32,
) -> i32 {
    let TbAppendRequest {
        scope,
        table,
        eid,
        value,
    } = tri!(decode(
        &mut caller,
        req_ptr,
        req_len,
        "table append request"
    ));

    let scope = tri!(transform_scope(&mut caller, scope));

    defer(
        &mut caller,
        tb_append::Op {
            scope,
            table,
            eid,
            value,
        },
    )
}

pub(crate) async fn tb_count(
    mut caller: Caller<'_, CellState>,
    req_ptr: u32,
    req_len: u32,
    rsp_ptr: u32,
    rsp_len: u32,
) -> i32 {
    let TbCountRequest { scope, table } =
        tri!(decode(&mut caller, req_ptr, req_len, "table count request"));

    let scope = tri!(transform_scope(&mut caller, scope));

    let counted = tri!(apply(&mut caller, tb_count::Op { scope, table }).await);
    let response = TbCountResponse {
        count: counted.count,
    };

    let () = tri!(encode(
        &mut caller,
        rsp_ptr,
        rsp_len,
        &response,
        "table count response"
    ));

    SUCCESS
}

pub(crate) async fn tb_get(
    mut caller: Caller<'_, CellState>,
    req_ptr: u32,
    req_len: u32,
    rsp_ptr: u32,
    rsp_len: u32,
) -> i32 {
    let TbGetRequest { scope, table, eid } =
        tri!(decode(&mut caller, req_ptr, req_len, "table get request"));

    let scope = tri!(transform_scope(&mut caller, scope));

    let got = tri!(apply(&mut caller, tb_get::Op { scope, table, eid }).await);
    let response = TbGetResponse { value: got.value };

    let () = tri!(encode(
        &mut caller,
        rsp_ptr,
        rsp_len,
        &response,
        "table get response"
    ));

    SUCCESS
}

pub(crate) async fn tb_list(
    mut caller: Caller<'_, CellState>,
    req_ptr: u32,
    req_len: u32,
    rsp_ptr: u32,
    rsp_len: u32,
) -> i32 {
    let TbListRequest {
        scope,
        table,
        cursor,
        limit,
        order,
    } = tri!(decode(&mut caller, req_ptr, req_len, "table list request"));

    let scope = tri!(transform_scope(&mut caller, scope));
    let cursor = cursor.map(transform_cursor);
    let order = order.map(transform_order);

    let listed = tri!(
        apply(
            &mut caller,
            tb_list::Op {
                scope,
                table,
                cursor,
                limit,
                order,
            },
        )
        .await
    );
    let response = TbListResponse {
        entities: listed.entities,
    };

    let () = tri!(encode(
        &mut caller,
        rsp_ptr,
        rsp_len,
        &response,
        "table list response"
    ));

    SUCCESS
}

pub(crate) async fn tb_delete(
    mut caller: Caller<'_, CellState>,
    payload_ptr: u32,
    payload_len: u32,
) -> i32 {
    let TbDeleteRequest { scope, table, eid } = tri!(decode(
        &mut caller,
        payload_ptr,
        payload_len,
        "table delete request"
    ));

    let scope = tri!(transform_scope(&mut caller, scope));

    defer(&mut caller, tb_delete::Op { scope, table, eid })
}
