//! Semantic datalayer host functions.

use core::ffi::c_int;

use db_client::v1::models::{sem_select, sem_update};
use myrmic_common::db::{SelectRequest, SelectResponse, UpdateRequest};
use wasm_runtime_macros::host_function;

use crate::imports::db::{apply, decode_request, defer, encode_response, transform_scope};
use crate::tri;

#[host_function]
fn sem_update(buffer: *const u8, length: c_int) -> c_int {
    let UpdateRequest {
        scope,
        query,
        base_iri,
    } = tri!(
        // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
        unsafe { decode_request(buffer, length, "sem-update request") }
    );

    let scope = tri!(transform_scope(scope));

    defer(sem_update::Op {
        scope,
        query,
        base_iri,
    })
}

#[host_function]
fn sem_select(
    req_buffer: *const u8,
    req_length: c_int,
    rsp_buffer: *mut u8,
    rsp_length: c_int,
) -> c_int {
    let SelectRequest {
        scope,
        query,
        base_iri,
        skip,
        limit,
    } = tri!(
        // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
        unsafe { decode_request(req_buffer, req_length, "sem-select request") }
    );

    let scope = tri!(transform_scope(scope));

    let sem_select::Response {
        variables,
        solutions,
    } = tri!(apply(sem_select::Op {
        scope,
        query,
        base_iri,
        limit,
        skip,
    }));

    // SAFETY: WAMR passes a valid guest-memory pointer and matching length for this call.
    unsafe {
        encode_response(
            rsp_buffer,
            rsp_length,
            &SelectResponse {
                variables,
                solutions,
            },
            "sem-select response",
        )
    }
}
