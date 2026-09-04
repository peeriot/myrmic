//! Async IO utilities

use embedded_io_async::{Error, ErrorKind, Read};

/// Read the whole stream into the provided buffer.
///
/// # Arguments
/// - `read`: The async reader to read from.
/// - `buf`: The buffer to read into.
/// - `overflow_err`: An optional error to return if the buffer overflows.
///
/// # Returns
/// - `Ok(usize)`: The number of bytes read.
/// - `Err(E)`: An error occurred during reading or buffer overflow.
pub async fn read_all<E, R: Read>(
    mut read: R,
    buf: &mut [u8],
    overflow_err: Option<E>,
) -> Result<usize, E>
where
    E: From<ErrorKind>,
{
    let mut total = 0;

    loop {
        if total == buf.len() {
            if let Some(overflow_err) = overflow_err {
                break Err(overflow_err);
            } else {
                break Ok(total);
            }
        }

        let len = read.read(&mut buf[total..]).await.map_err(|e| e.kind())?;
        if len == 0 {
            break Ok(total);
        }

        total += len;
    }
}
