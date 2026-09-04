//! Base64 encoding and decoding utilities.

use base64::Engine;

use crate::utils::BufferOverflowError;

/// Encode data to base64, writing the result into the provided buffer.
///
/// # Arguments
/// - `data`: The data to encode.
/// - `buf`: A mutable byte slice to write the base64 encoded data into.
///
/// # Returns
/// - `Ok((str, buf))`: The base64 encoded data as a UTF-8 string and the remaining buffer space.
/// - `Err(BufferOverflowError)`: The buffer was not large enough to hold the encoded data.
pub fn base64_encode<'a>(
    data: &[u8],
    buf: &'a mut [u8],
) -> Result<(&'a str, &'a mut [u8]), BufferOverflowError> {
    let len = base64::engine::general_purpose::STANDARD_NO_PAD
        .encode_slice(data, buf)
        .map_err(|_| BufferOverflowError)?;

    let (str_buf, rem_buf) = buf.split_at_mut(len);

    let str = core::str::from_utf8(str_buf).unwrap(); // Safe, as base64 is always valid UTF-8

    Ok((str, rem_buf))
}

/// Decode base64 data, writing the result into the provided buffer.
///
/// # Arguments
/// - `data`: The base64 encoded data to decode.
/// - `buf`: A mutable byte slice to write the decoded data into.
///
/// # Returns
/// - `Ok((data, buf))`: The decoded data and the remaining buffer space.
/// - `Err(BufferOverflowError)`: The buffer was not large enough to hold the decoded data or the input was not valid base64.
pub fn base64_decode<'a>(
    data: &str,
    buf: &'a mut [u8],
) -> Result<(&'a [u8], &'a mut [u8]), BufferOverflowError> {
    let len = base64::engine::general_purpose::STANDARD_NO_PAD
        .decode_slice(data, buf)
        .map_err(|_| BufferOverflowError)?;

    let (data_buf, rem_buf) = buf.split_at_mut(len);

    Ok((data_buf, rem_buf))
}
