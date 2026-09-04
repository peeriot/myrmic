use db_client::v1::models::{key_delete, key_get, key_prefix, key_put};
use myrmic_common::db::{
    DeleteRequest, GetRequest, GetResponse, PrefixRequest, PrefixResponse, PutRequest,
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

pub(crate) async fn key_put(
    mut caller: Caller<'_, CellState>,
    payload_ptr: u32,
    payload_len: u32,
) -> i32 {
    let PutRequest { scope, key, value } = tri!(decode(
        &mut caller,
        payload_ptr,
        payload_len,
        "key-value put request"
    ));

    let scope = tri!(transform_scope(&mut caller, scope));

    defer(&mut caller, key_put::Op { scope, key, value })
}

pub(crate) async fn key_delete(
    mut caller: Caller<'_, CellState>,
    payload_ptr: u32,
    payload_len: u32,
) -> i32 {
    let DeleteRequest { scope, key } = tri!(decode(
        &mut caller,
        payload_ptr,
        payload_len,
        "key-value delete request"
    ));

    let scope = tri!(transform_scope(&mut caller, scope));

    defer(&mut caller, key_delete::Op { scope, key })
}

pub(crate) async fn key_get(
    mut caller: Caller<'_, CellState>,
    req_ptr: u32,
    req_len: u32,
    rsp_ptr: u32,
    rsp_len: u32,
) -> i32 {
    let GetRequest { scope, key } = tri!(decode(
        &mut caller,
        req_ptr,
        req_len,
        "key-value get request"
    ));

    let scope = tri!(transform_scope(&mut caller, scope));

    let got = tri!(apply(&mut caller, key_get::Op { scope, key }).await);
    let response = GetResponse { payload: got.value };

    let () = tri!(encode(
        &mut caller,
        rsp_ptr,
        rsp_len,
        &response,
        "key-value get response"
    ));

    SUCCESS
}

pub(crate) async fn key_prefix(
    mut caller: Caller<'_, CellState>,
    req_ptr: u32,
    req_len: u32,
    rsp_ptr: u32,
    rsp_len: u32,
) -> i32 {
    let PrefixRequest { scope, prefix } = tri!(decode(
        &mut caller,
        req_ptr,
        req_len,
        "key-value prefix request"
    ));

    let scope = tri!(transform_scope(&mut caller, scope));

    let listed = tri!(apply(&mut caller, key_prefix::Op { scope, prefix }).await);
    let response = PrefixResponse { keys: listed.keys };

    let () = tri!(encode(
        &mut caller,
        rsp_ptr,
        rsp_len,
        &response,
        "key-value prefix response"
    ));

    SUCCESS
}
