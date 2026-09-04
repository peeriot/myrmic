//! Host functions used to interact with the datalayer using its key-value API

use alloc::string::String;
use alloc::vec::Vec;
use myrmic_common::db::{
    DeleteRequest, GetRequest, GetResponse, PrefixRequest, PrefixResponse, PutRequest, Scope,
};

use crate::error::ErrorCode;
use crate::{ApiError, ApiResult};

mod c_functions {
    use core::ffi::c_int;

    #[link(wasm_import_module = "db")]
    unsafe extern "C" {

        /// Store a value under a certain key
        /// NOTE that
        /// the writing / publishing is implemented as a transaction with the write as its only operation
        ///
        /// # Arguments:
        /// - buffer: pointer to the buffer containing the serialized `PutRequest`
        /// - length: length of the buffer
        ///
        /// # Returns:
        /// - [`crate::SUCCESS`] on success
        /// - TODO on error
        pub(super) fn key_put(buffer: *const u8, length: c_int) -> c_int;

        /// Retrieve the value stored with the provided key, if any.
        /// NOTE that
        /// the read is implemented as a transaction with the read as its only operation
        ///
        /// # Arguments:
        /// - `buffer_req`: pointer to the buffer containing the serialized `GetRequest`
        /// - `let_req`: length of the buffer containing the `GetRequest`
        /// - `buffer_rsp`: pointer to a location in module memory where the host can write the find response
        /// - `len_rsp`: maximal number of bytes that can be written as response
        ///
        /// # Returns:
        /// - [`crate::SUCCESS`] on success
        /// - TODO on error
        pub(super) fn key_get(
            buffer_req: *const u8,
            len_req: c_int,
            buffer_rsp: *mut u8,
            len_rsp: c_int,
        ) -> c_int;

        /// Delete the value stored with the provided key, if any.
        /// NOTE that
        /// the write is implemented as a transaction with the write as its only operation
        ///
        /// # Arguments:
        /// - `buffer_req`: pointer to the buffer containing the serialized `DeleteRequest`
        /// - `let_req`: length of the buffer containing the `DeleteRequest`
        ///
        /// # Returns:
        /// - [`crate::SUCCESS`] on success
        /// - TODO on error
        pub(super) fn key_delete(buffer_req: *const u8, len_req: c_int) -> c_int;

        /// Retrieve the keys whose value is stored under the provided prefix, if any.
        /// NOTE that
        /// the read is implemented as a transaction with the read as its only operation
        ///
        /// # Arguments:
        /// - `buffer_req`: pointer to the buffer containing the serialized `PrefixRequest`
        /// - `let_req`: length of the buffer containing the `PrefixRequest`
        /// - `buffer_rsp`: pointer to a location in module memory where the host can write the find response
        /// - `len_rsp`: maximal number of bytes that can be written as response
        ///
        /// # Returns:
        /// - [`crate::SUCCESS`] on success
        /// - TODO on error
        pub(super) fn key_prefix(
            buffer_req: *const u8,
            len_req: c_int,
            buffer_rsp: *mut u8,
            len_rsp: c_int,
        ) -> c_int;

    }
}

/// Stores `value` under `key` in `scope`, replacing any existing value.
///
/// `buffer` is caller-provided scratch for the serialized request. Prefer the
/// typed [`Kv`](super::tree::Kv) handle, which manages buffers for you.
pub fn key_put(scope: Scope, key: String, value: Vec<u8>, buffer: &mut [u8]) -> ApiResult<()> {
    let payload = PutRequest { scope, key, value };
    let len = postcard::to_slice(&payload, buffer)
        .map_err(|_e| ApiError::Serde("serializing put request"))?
        .len();
    // SAFETY: calling the imported function with pointer and length of guest linear memory
    // used for this specific purpose.
    unsafe { c_functions::key_put(buffer.as_ptr(), len as i32) }.to_result()
}

/// Fetches the value stored under `key` in `scope`, or `None` if the key is
/// absent.
///
/// `request_buffer` and `response_buffer` are caller-provided scratch; the
/// response must fit `response_buffer`.
pub fn key_get(
    scope: Scope,
    key: String,
    request_buffer: &mut [u8],
    response_buffer: &mut [u8],
) -> ApiResult<Option<Vec<u8>>> {
    // note: in principle, we could also re-use the buffer, but we'll keep optimization for later

    let request = GetRequest { scope, key };
    let payload_len = postcard::to_slice(&request, request_buffer)
        .map_err(|_e| ApiError::Serde("serializing get request"))?
        .len();

    // SAFETY: calling the imported function with pointer and length of guest linear memory
    // used for this specific purpose.
    unsafe {
        c_functions::key_get(
            request_buffer.as_ptr(),
            payload_len as i32,
            response_buffer.as_mut_ptr(),
            response_buffer.len() as i32,
        )
    }
    .to_result()?;

    let response = postcard::from_bytes::<GetResponse>(response_buffer)
        .map_err(|_e| ApiError::Serde("deserializing get response"))?;
    Ok(response.payload)
}

/// Deletes the value stored under `key` in `scope`, if any.
///
/// `buffer` is caller-provided scratch for the serialized request.
pub fn key_delete(scope: Scope, key: String, buffer: &mut [u8]) -> ApiResult<()> {
    let delete_request = DeleteRequest { scope, key };
    let payload_len = postcard::to_slice(&delete_request, buffer)
        .map_err(|_e| ApiError::Serde("serializing delete request"))?
        .len();

    // SAFETY: calling the imported function with pointer and length of guest linear memory
    // used for this specific purpose.
    unsafe { c_functions::key_delete(buffer.as_ptr(), payload_len as i32) }.to_result()
}

/// Lists the full keys stored under `prefix` in `scope`.
///
/// `request_buffer` and `response_buffer` are caller-provided scratch; the
/// response must fit `response_buffer`.
pub fn key_prefix(
    scope: Scope,
    prefix: String,
    request_buffer: &mut [u8],
    response_buffer: &mut [u8],
) -> ApiResult<Vec<String>> {
    // note: in principle, we could also re-use the buffer, but we'll keep optimization for later

    let request = PrefixRequest { scope, prefix };
    let payload_len = postcard::to_slice(&request, request_buffer)
        .map_err(|_e| ApiError::Serde("serializing prefix request"))?
        .len();

    // SAFETY: calling the imported function with pointer and length of guest linear memory
    // used for this specific purpose.
    unsafe {
        c_functions::key_prefix(
            request_buffer.as_ptr(),
            payload_len as i32,
            response_buffer.as_mut_ptr(),
            response_buffer.len() as i32,
        )
    }
    .to_result()?;

    let response = postcard::from_bytes::<PrefixResponse>(response_buffer)
        .map_err(|_e| ApiError::Serde("deserializing prefix response"))?;
    Ok(response.keys)
}
