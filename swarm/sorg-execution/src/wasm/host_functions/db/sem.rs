use db_client::v1::models::sem_select;
use myrmic_common::db::{SelectRequest, SelectResponse, UpdateRequest};
use myrmic_common::types::error::SUCCESS;
use wasmtime::Caller;

use crate::wasm::{
    cell::state::CellState,
    host_functions::{
        db::{apply, defer, transform_scope},
        decode, encode, tri,
    },
};

pub(crate) async fn sem_update(
    mut caller: Caller<'_, CellState>,
    payload_ptr: u32,
    payload_len: u32,
) -> i32 {
    let UpdateRequest {
        scope,
        query,
        base_iri,
    } = tri!(decode(
        &mut caller,
        payload_ptr,
        payload_len,
        "sem-update request"
    ));

    let scope = tri!(transform_scope(&mut caller, scope));

    defer(
        &mut caller,
        db_client::v1::models::sem_update::Op {
            scope,
            query,
            base_iri,
        },
    )
}

pub(crate) async fn sem_select(
    mut caller: Caller<'_, CellState>,
    req_ptr: u32,
    req_len: u32,
    rsp_ptr: u32,
    rsp_len: u32,
) -> i32 {
    let SelectRequest {
        scope,
        query,
        base_iri,
        skip,
        limit,
    } = tri!(decode(&mut caller, req_ptr, req_len, "sem-select request"));

    let scope = tri!(transform_scope(&mut caller, scope));

    let resp = tri!(
        apply(
            &mut caller,
            sem_select::Op {
                scope,
                query,
                base_iri,
                skip,
                limit,
            },
        )
        .await
    );

    let response = transform_response(resp);

    let () = tri!(encode(
        &mut caller,
        rsp_ptr,
        rsp_len,
        &response,
        "sem-select response"
    ));

    SUCCESS
}

type DbSelectResponse = sem_select::Response;
type WasmSelectResponse = SelectResponse;

fn transform_response(response: DbSelectResponse) -> WasmSelectResponse {
    WasmSelectResponse {
        solutions: response.solutions,
        variables: response.variables,
    }
}
