//! 4-byte LE length-prefixed postcard framing with `64 KiB` cap.
//!
//! Oversized or unparseable frames fail closed (return an error; callers
//! must drop the connection on any `FrameError`).

use std::io;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::MAX_FRAME_LEN;

/// Errors from frame read/write.
#[derive(Debug, Error)]
pub enum FrameError {
    #[error("frame length {0} exceeds the 64 KiB cap")]
    TooLarge(u32),
    #[error("postcard decode error: {0}")]
    Decode(#[from] postcard::Error),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Read one postcard frame.  Returns the raw payload bytes.
///
/// Fails with [`FrameError::TooLarge`] if the declared length exceeds
/// [`MAX_FRAME_LEN`], or [`FrameError::Io`] on EOF / IO problems.
pub async fn read_frame<R>(reader: &mut R) -> Result<Vec<u8>, FrameError>
where
    R: AsyncRead + Unpin,
{
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf);
    if len > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge(len));
    }
    let mut payload = vec![0u8; len as usize];
    reader.read_exact(&mut payload).await?;
    Ok(payload)
}

/// Write one postcard-serialized value as a length-prefixed frame.
///
/// The length prefix and payload are serialized into a single buffer and sent
/// with one `write_all` call, ensuring the frame is written atomically — a
/// cancellation or partial-write between prefix and payload cannot desync the
/// stream.
pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: serde::Serialize,
{
    let payload = postcard::to_allocvec(value)?;
    let len = u32::try_from(payload.len()).map_err(|_| FrameError::TooLarge(u32::MAX))?;
    // Serialize length prefix + payload into one contiguous buffer so that a
    // single write_all is all-or-nothing (B2b).
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(&payload);
    writer.write_all(&frame).await?;
    Ok(())
}

/// Decode a postcard-encoded value from raw frame bytes.
pub fn decode_frame<'a, T>(bytes: &'a [u8]) -> Result<T, FrameError>
where
    T: serde::Deserialize<'a>,
{
    postcard::from_bytes(bytes).map_err(FrameError::Decode)
}
