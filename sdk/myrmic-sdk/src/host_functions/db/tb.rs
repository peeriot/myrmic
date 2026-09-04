//! Host functions used to interact with the datalayer using its table API

use alloc::string::String;
use alloc::vec::Vec;
use myrmic_common::db::{
    Cursor, Scope, TbAppendRequest, TbCountRequest, TbCountResponse, TbDeleteRequest, TbGetRequest,
    TbGetResponse, TbInsertRequest, TbInsertResponse, TbListRequest, TbListResponse, TbOrderBy,
};

use crate::error::ErrorCode;
use crate::{ApiError, ApiResult};

mod c_functions {
    use core::ffi::c_int;

    #[link(wasm_import_module = "db")]
    unsafe extern "C" {

        /// Insert an entity into a table, allocating an id if none is provided.
        /// NOTE that
        /// the write is implemented as a transaction with the write as its only operation
        ///
        /// # Arguments:
        /// - `buffer_req`: pointer to the buffer containing the serialized `TbInsertRequest`
        /// - `let_req`: length of the buffer containing the `TbInsertRequest`
        /// - `buffer_rsp`: pointer to a location in module memory where the host can write the find response
        /// - `len_rsp`: maximal number of bytes that can be written as response
        ///
        /// # Returns:
        /// - [`crate::SUCCESS`] on success
        /// - TODO on error
        pub(super) fn tb_insert(
            buffer_req: *const u8,
            len_req: c_int,
            buffer_rsp: *mut u8,
            len_rsp: c_int,
        ) -> c_int;

        /// Count the number of entities stored in a table.
        /// NOTE that
        /// the read is implemented as a transaction with the read as its only operation
        ///
        /// # Arguments:
        /// - `buffer_req`: pointer to the buffer containing the serialized `TbCountRequest`
        /// - `let_req`: length of the buffer containing the `TbCountRequest`
        /// - `buffer_rsp`: pointer to a location in module memory where the host can write the find response
        /// - `len_rsp`: maximal number of bytes that can be written as response
        ///
        /// # Returns:
        /// - [`crate::SUCCESS`] on success
        /// - TODO on error
        pub(super) fn tb_count(
            buffer_req: *const u8,
            len_req: c_int,
            buffer_rsp: *mut u8,
            len_rsp: c_int,
        ) -> c_int;

        /// Retrieve the entity stored under the provided id in a table, if any.
        /// NOTE that
        /// the read is implemented as a transaction with the read as its only operation
        ///
        /// # Arguments:
        /// - `buffer_req`: pointer to the buffer containing the serialized `TbGetRequest`
        /// - `let_req`: length of the buffer containing the `TbGetRequest`
        /// - `buffer_rsp`: pointer to a location in module memory where the host can write the find response
        /// - `len_rsp`: maximal number of bytes that can be written as response
        ///
        /// # Returns:
        /// - [`crate::SUCCESS`] on success
        /// - TODO on error
        pub(super) fn tb_get(
            buffer_req: *const u8,
            len_req: c_int,
            buffer_rsp: *mut u8,
            len_rsp: c_int,
        ) -> c_int;

        /// List the entities stored in a table, optionally from a cursor and bounded by a limit.
        /// NOTE that
        /// the read is implemented as a transaction with the read as its only operation
        ///
        /// # Arguments:
        /// - `buffer_req`: pointer to the buffer containing the serialized `TbListRequest`
        /// - `let_req`: length of the buffer containing the `TbListRequest`
        /// - `buffer_rsp`: pointer to a location in module memory where the host can write the find response
        /// - `len_rsp`: maximal number of bytes that can be written as response
        ///
        /// # Returns:
        /// - [`crate::SUCCESS`] on success
        /// - TODO on error
        pub(super) fn tb_list(
            buffer_req: *const u8,
            len_req: c_int,
            buffer_rsp: *mut u8,
            len_rsp: c_int,
        ) -> c_int;

        /// Delete the entity stored under the provided id in a table, if any.
        /// NOTE that
        /// the write is implemented as a transaction with the write as its only operation
        ///
        /// # Arguments:
        /// - `buffer_req`: pointer to the buffer containing the serialized `TbDeleteRequest`
        /// - `let_req`: length of the buffer containing the `TbDeleteRequest`
        ///
        /// # Returns:
        /// - [`crate::SUCCESS`] on success
        /// - TODO on error
        pub(super) fn tb_delete(buffer_req: *const u8, len_req: c_int) -> c_int;

        /// Insert an entity without being told the id it landed under.
        ///
        /// # Arguments:
        /// - `buffer_req`: pointer to the buffer containing the serialized `TbAppendRequest`
        /// - `len_req`: length of the buffer containing the `TbAppendRequest`
        ///
        /// # Returns:
        /// - [`crate::SUCCESS`] on success
        /// - TODO on error
        pub(super) fn tb_append(buffer_req: *const u8, len_req: c_int) -> c_int;

    }
}

/// Inserts `value` into `table` in `scope`, keyed by `eid` — or by a
/// host-generated entity id when `eid` is `None` — and returns the entity id
/// the row is stored under.
///
/// `request_buffer` and `response_buffer` are caller-provided scratch. Prefer
/// the typed [`Table`](super::table::Table) handle, which manages buffers for
/// you.
pub fn tb_insert(
    scope: Scope,
    table: String,
    eid: Option<Vec<u8>>,
    value: Vec<u8>,
    request_buffer: &mut [u8],
    response_buffer: &mut [u8],
) -> ApiResult<Vec<u8>> {
    let request = TbInsertRequest {
        scope,
        table,
        eid,
        value,
    };
    let payload_len = postcard::to_slice(&request, request_buffer)
        .map_err(|_e| ApiError::Serde("serializing table insert request"))?
        .len();

    // SAFETY: calling the imported function with pointer and length of guest linear memory
    // used for this specific purpose.
    unsafe {
        c_functions::tb_insert(
            request_buffer.as_ptr(),
            payload_len as i32,
            response_buffer.as_mut_ptr(),
            response_buffer.len() as i32,
        )
    }
    .to_result()?;

    let response = postcard::from_bytes::<TbInsertResponse>(response_buffer)
        .map_err(|_e| ApiError::Serde("deserializing table insert response"))?;
    Ok(response.eid)
}

/// Returns the number of rows in `table` in `scope`.
///
/// `request_buffer` and `response_buffer` are caller-provided scratch.
pub fn tb_count(
    scope: Scope,
    table: String,
    request_buffer: &mut [u8],
    response_buffer: &mut [u8],
) -> ApiResult<usize> {
    let request = TbCountRequest { scope, table };
    let payload_len = postcard::to_slice(&request, request_buffer)
        .map_err(|_e| ApiError::Serde("serializing table count request"))?
        .len();

    // SAFETY: calling the imported function with pointer and length of guest linear memory
    // used for this specific purpose.
    unsafe {
        c_functions::tb_count(
            request_buffer.as_ptr(),
            payload_len as i32,
            response_buffer.as_mut_ptr(),
            response_buffer.len() as i32,
        )
    }
    .to_result()?;

    let response = postcard::from_bytes::<TbCountResponse>(response_buffer)
        .map_err(|_e| ApiError::Serde("deserializing table count response"))?;
    Ok(response.count)
}

/// Fetches the row keyed `eid` from `table` in `scope`, or `None` if there is
/// no such row.
///
/// `request_buffer` and `response_buffer` are caller-provided scratch.
pub fn tb_get(
    scope: Scope,
    table: String,
    eid: Vec<u8>,
    request_buffer: &mut [u8],
    response_buffer: &mut [u8],
) -> ApiResult<Option<Vec<u8>>> {
    let request = TbGetRequest { scope, table, eid };
    let payload_len = postcard::to_slice(&request, request_buffer)
        .map_err(|_e| ApiError::Serde("serializing table get request"))?
        .len();

    // SAFETY: calling the imported function with pointer and length of guest linear memory
    // used for this specific purpose.
    unsafe {
        c_functions::tb_get(
            request_buffer.as_ptr(),
            payload_len as i32,
            response_buffer.as_mut_ptr(),
            response_buffer.len() as i32,
        )
    }
    .to_result()?;

    let response = postcard::from_bytes::<TbGetResponse>(response_buffer)
        .map_err(|_e| ApiError::Serde("deserializing table get response"))?;
    Ok(response.value)
}

/// Lists `(eid, value)` rows from `table` in `scope` in the given `order`,
/// starting from `cursor` and truncated to `limit`.
///
/// `request_buffer` and `response_buffer` are caller-provided scratch; the
/// listed rows must fit `response_buffer`.
#[allow(clippy::too_many_arguments)]
pub fn tb_list(
    scope: Scope,
    table: String,
    cursor: Option<Cursor>,
    limit: Option<usize>,
    order: Option<TbOrderBy>,
    request_buffer: &mut [u8],
    response_buffer: &mut [u8],
) -> ApiResult<Vec<(Vec<u8>, Vec<u8>)>> {
    let request = TbListRequest {
        scope,
        table,
        cursor,
        limit,
        order,
    };
    let payload_len = postcard::to_slice(&request, request_buffer)
        .map_err(|_e| ApiError::Serde("serializing table list request"))?
        .len();

    // SAFETY: calling the imported function with pointer and length of guest linear memory
    // used for this specific purpose.
    unsafe {
        c_functions::tb_list(
            request_buffer.as_ptr(),
            payload_len as i32,
            response_buffer.as_mut_ptr(),
            response_buffer.len() as i32,
        )
    }
    .to_result()?;

    let response = postcard::from_bytes::<TbListResponse>(response_buffer)
        .map_err(|_e| ApiError::Serde("deserializing table list response"))?;
    Ok(response.entities)
}

/// Inserts without asking for the id back, which is what lets the host apply it
/// alongside the rest of the handler's writes instead of in a round trip of its
/// own.
///
/// `buffer` is caller-provided scratch for the serialized request.
pub fn tb_append(
    scope: Scope,
    table: String,
    eid: Option<Vec<u8>>,
    value: Vec<u8>,
    buffer: &mut [u8],
) -> ApiResult<()> {
    let request = TbAppendRequest {
        scope,
        table,
        eid,
        value,
    };
    let payload_len = postcard::to_slice(&request, buffer)
        .map_err(|_e| ApiError::Serde("serializing table append request"))?
        .len();

    // SAFETY: calling the imported function with pointer and length of guest linear memory
    // used for this specific purpose.
    unsafe { c_functions::tb_append(buffer.as_ptr(), payload_len as i32) }.to_result()
}

/// Deletes the row keyed `eid` from `table` in `scope`, if any.
///
/// `buffer` is caller-provided scratch for the serialized request.
pub fn tb_delete(scope: Scope, table: String, eid: Vec<u8>, buffer: &mut [u8]) -> ApiResult<()> {
    let request = TbDeleteRequest { scope, table, eid };
    let payload_len = postcard::to_slice(&request, buffer)
        .map_err(|_e| ApiError::Serde("serializing table delete request"))?
        .len();

    // SAFETY: calling the imported function with pointer and length of guest linear memory
    // used for this specific purpose.
    unsafe { c_functions::tb_delete(buffer.as_ptr(), payload_len as i32) }.to_result()
}
