//! Host functions used to interact with the datalayer using its semantic API

use crate::{ApiError, ApiResult, error::ErrorCode};
use alloc::string::String;
use myrmic_common::db::{Scope, SelectRequest, SelectResponse, UpdateRequest};

mod c_functions {
    use core::ffi::c_int;

    #[link(wasm_import_module = "db")]
    unsafe extern "C" {

        /// Make an update query to the sem store
        /// NOTE that
        /// the write is implemented as a transaction with the write as its only operation
        ///
        /// # Arguments:
        /// - buffer: pointer to the buffer containing the serialized `UpdateRequest`
        /// - length: length of the buffer
        ///
        /// # Returns:
        /// - [`crate::SUCCESS`] on success
        /// - TODO on error
        pub(super) fn sem_update(buffer: *const u8, length: c_int) -> c_int;

        /// Makes an select query to the sem store and writes the query result into the provided buffer
        /// NOTE that
        /// the read is implemented as a transaction with the read as its only operation
        ///
        /// # Arguments:
        /// - `buffer_req`: pointer to the buffer containing the serialized `SelectRequest`
        /// - `let_req`: length of the buffer containing the `SelectRequest`
        /// - `buffer_rsp`: pointer to a location in module memory where the host can write the select response
        /// - `len_rsp`: maximal number of bytes that can be written as response
        ///
        /// # Returns:
        /// - [`crate::SUCCESS`] on success
        /// - TODO on error
        pub(super) fn sem_select(
            buffer_req: *const u8,
            len_req: c_int,
            buffer_rsp: *mut u8,
            len_rsp: c_int,
        ) -> c_int;
    }
}

// for now, we are working with constant-size buffers that we allocate on the stack
// we could later extend the api to allow the user to specify the maximal size of buffers
const MAX_SIZE_SEM_BUFFER: usize = 1000;

/// Runs an update `query` against the semantic store in `scope`.
///
/// `base_iri`, when given, resolves relative IRIs in the query.
pub fn sem_update(scope: Scope, query: String, base_iri: Option<String>) -> ApiResult<()> {
    let mut request_buffer = [0u8; MAX_SIZE_SEM_BUFFER];

    let payload = UpdateRequest {
        scope,
        query,
        base_iri,
    };
    let len = postcard::to_slice(&payload, &mut request_buffer)
        .map_err(|_e| ApiError::Serde("serializing update request"))?
        .len();
    // SAFETY: calling the imported function with pointer and length of guest linear memory
    // used for this specific purpose.
    unsafe { c_functions::sem_update(request_buffer.as_ptr(), len as i32) }.to_result()
}

/// Runs a select `query` against the semantic store in `scope` and returns the
/// matching rows, paged by `limit` and `skip`. A `None` `limit` is not
/// unlimited: the host applies its own default page size of 100 solutions.
///
/// Request and response travel through fixed 1000-byte buffers, so an oversized
/// query or page fails the call instead of truncating. Keep `limit` small
/// enough for a page to fit and walk the result with `skip`.
///
/// `base_iri`, when given, resolves relative IRIs in the query.
pub fn sem_select(
    scope: Scope,
    query: String,
    base_iri: Option<String>,
    limit: Option<usize>,
    skip: Option<usize>,
) -> ApiResult<SelectResponse> {
    // note: in principle, we could also re-use the buffer, but we'll keep optimization for later
    let mut request_buffer = [0u8; MAX_SIZE_SEM_BUFFER];
    let mut response_buffer = [0u8; MAX_SIZE_SEM_BUFFER];

    let request = SelectRequest {
        scope,
        query,
        base_iri,
        limit,
        skip,
    };
    let payload_len = postcard::to_slice(&request, &mut request_buffer)
        .map_err(|_e| ApiError::Serde("serializing select request"))?
        .len();

    // SAFETY: calling the imported function with pointer and length of guest linear memory
    // used for this specific purpose.
    unsafe {
        c_functions::sem_select(
            request_buffer.as_ptr(),
            payload_len as i32,
            response_buffer.as_mut_ptr(),
            MAX_SIZE_SEM_BUFFER as i32,
        )
    }
    .to_result()?;

    let response = postcard::from_bytes::<SelectResponse>(&response_buffer)
        .map_err(|_e| ApiError::Serde("deserializing select response"))?;
    Ok(response)
}
