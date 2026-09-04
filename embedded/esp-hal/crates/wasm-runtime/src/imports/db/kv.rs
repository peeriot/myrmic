//! Key-value datalayer host functions.

use core::ffi::c_int;

use db_client::v1::models::{key_delete, key_get, key_prefix, key_put};
use myrmic_common::db::{
    DeleteRequest, GetRequest, GetResponse, PrefixRequest, PrefixResponse, PutRequest,
};
use wasm_runtime_macros::host_function;

use crate::imports::db::{apply, decode_request, defer, encode_response, transform_scope};
use crate::tri;

#[host_function]
fn key_put(buffer: *const u8, length: c_int) -> c_int {
    let PutRequest { scope, key, value } = tri!(
        // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
        unsafe { decode_request(buffer, length, "key-value put request") }
    );

    let scope = tri!(transform_scope(scope));

    defer(key_put::Op { scope, key, value })
}

#[host_function]
fn key_delete(buffer: *const u8, length: c_int) -> c_int {
    let DeleteRequest { scope, key } = tri!(
        // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
        unsafe { decode_request(buffer, length, "key-value delete request") }
    );

    let scope = tri!(transform_scope(scope));

    defer(key_delete::Op { scope, key })
}

#[host_function]
fn key_get(
    req_buffer: *const u8,
    req_length: c_int,
    rsp_buffer: *mut u8,
    rsp_length: c_int,
) -> c_int {
    let GetRequest { scope, key } = tri!(
        // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
        unsafe { decode_request(req_buffer, req_length, "key-value get request") }
    );

    let scope = tri!(transform_scope(scope));

    let key_get::Response { value } = tri!(apply(key_get::Op { scope, key }));

    // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
    unsafe {
        encode_response(
            rsp_buffer,
            rsp_length,
            &GetResponse { payload: value },
            "key-value get response",
        )
    }
}

#[host_function]
fn key_prefix(
    req_buffer: *const u8,
    req_length: c_int,
    rsp_buffer: *mut u8,
    rsp_length: c_int,
) -> c_int {
    let PrefixRequest { scope, prefix } = tri!(
        // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
        unsafe { decode_request(req_buffer, req_length, "key-value prefix request") }
    );

    let scope = tri!(transform_scope(scope));

    let key_prefix::Response { keys } = tri!(apply(key_prefix::Op { scope, prefix }));

    // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
    unsafe {
        encode_response(
            rsp_buffer,
            rsp_length,
            &PrefixResponse { keys },
            "key-value prefix response",
        )
    }
}
