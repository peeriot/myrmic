use db_client::v1::models::sem_select;
use myrmic_common::db::{SelectRequest, SelectResponse, UpdateRequest};
use myrmic_common::types::error::{EINVAL, SUCCESS};
use wasmtime::Caller;

use crate::wasm::{
    cell::state::CellState,
    host_functions::{
        db::{apply, defer, transform_scope},
        decode, encode, tri,
    },
};

/// Longest SPARQL text a cell may submit. The db bounds how deeply a query
/// nests on its own; this bounds the work a single call can ask for.
const MAX_QUERY_LEN: usize = 64 * 1024;

fn query_len(query: &str) -> Result<(), i32> {
    if query.len() > MAX_QUERY_LEN {
        return Err(EINVAL);
    }

    Ok(())
}

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

    tri!(query_len(&query));
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

    tri!(query_len(&query));
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

#[cfg(test)]
mod tests {
    use super::{EINVAL, MAX_QUERY_LEN, query_len};

    #[test]
    fn query_length_is_capped() {
        assert_eq!(query_len(&"a".repeat(MAX_QUERY_LEN)), Ok(()));
        assert_eq!(query_len(&"a".repeat(MAX_QUERY_LEN + 1)), Err(EINVAL));
    }
}
