//! Host functions used to interact with the data layer using its time-series API

use myrmic_common::db::{FindRequest, FindResponse, Measurement, PublishRequest, Scope, TsOrderBy};

use crate::{ApiError, ApiResult, db::MAX_SIZE_COMM_BUFFER, error::ErrorCode};

use alloc::string::String;

mod c_functions {
    use core::ffi::c_int;

    #[link(wasm_import_module = "db")]
    unsafe extern "C" {

        /// Publish a time-series measurement specified by the provided `PublishRequest`.
        /// NOTE that
        /// (1) the measurement is published at the current point in time (the host will take a timestamp)
        /// (2) the writing / publishing of the measurement is implemented as a transaction with the measurement write as its only operation
        ///
        /// # Arguments:
        /// - buffer: pointer to the buffer containing the serialized `PublishRequest`
        /// - length: length of the buffer
        ///
        /// # Returns:
        /// - [`crate::SUCCESS`] on success
        /// - TODO on error
        pub(super) fn publish_measurement(buffer: *const u8, length: c_int) -> c_int;

        /// Find the measurement samples described by the provided `FindRequest`.
        /// NOTE that
        /// the read of the measurement is implemented as a transaction with the measurement read as its only operation
        ///
        /// # Arguments:
        /// - `buffer_req`: pointer to the buffer containing the serialized `FindRequest`
        /// - `let_req`: length of the buffer containing the `FindRequest`
        /// - `buffer_rsp`: pointer to a location in module memory where the host can write the find response
        /// - `len_rsp`: maximal number of bytes that can be written as response
        ///
        /// # Returns:
        /// - [`crate::SUCCESS`] on success
        /// - TODO on error
        pub(super) fn find_measurement(
            buffer_req: *const u8,
            len_req: c_int,
            buffer_rsp: *mut u8,
            len_rsp: c_int,
        ) -> c_int;

    }
}

/// Writes `measurement` into the time-series store in `scope`.
pub fn publish_measurement(scope: Scope, measurement: Measurement) -> ApiResult<()> {
    let mut request_buffer = [0u8; MAX_SIZE_COMM_BUFFER];

    let payload = PublishRequest { scope, measurement };
    let len = postcard::to_slice(&payload, &mut request_buffer)
        .map_err(|_e| ApiError::Serde("serializing publish request"))?
        .len();
    // SAFETY: calling the imported function with pointer and length of guest linear memory
    // used for this specific purpose.
    unsafe { c_functions::publish_measurement(request_buffer.as_ptr(), len as i32) }.to_result()
}

/// Queries the time-series store in `scope` for samples of `measurement_name`,
/// bounded by the optional `start`/`end` timestamps, truncated to `limit`, in
/// the given `order`.
///
/// `req_buffer` and `resp_buffer` are caller-provided scratch; the matching
/// samples must fit `resp_buffer`.
#[allow(clippy::too_many_arguments)]
pub fn find_measurement(
    scope: Scope,
    measurement_name: String,
    limit: Option<usize>,
    start: Option<u64>,
    end: Option<u64>,
    order: Option<TsOrderBy>,
    req_buffer: &mut [u8],
    resp_buffer: &mut [u8],
) -> ApiResult<FindResponse> {
    let find_request = FindRequest {
        scope,
        measurement_name,
        limit,
        start,
        end,
        order,
    };
    let req_len = postcard::to_slice(&find_request, req_buffer)
        .map_err(|_err| ApiError::Serde("serializing find request"))?
        .len();

    // SAFETY: calling the imported function with pointer and length of guest linear memory
    // used for this specific purpose.
    unsafe {
        c_functions::find_measurement(
            req_buffer.as_ptr(),
            req_len as i32,
            resp_buffer.as_mut_ptr(),
            resp_buffer.len() as i32,
        )
    }
    .to_result()?;

    let response = postcard::from_bytes::<FindResponse>(resp_buffer)
        .map_err(|_err| ApiError::Serde("deserializing find response"))?;
    Ok(response)
}
