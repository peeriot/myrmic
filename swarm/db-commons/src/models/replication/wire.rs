//! On-disk snapshot format (`MYRCSNAP` v1).
//!
//! ```text
//! MAGIC      8 bytes   b"MYRCSNAP"
//! VERSION    1 byte    format version
//! HDR_LEN    u32 LE    length of the postcard-encoded header
//! HEADER     bytes     postcard(Header)
//! repeated `chunk_count` times:
//!   CHUNK_LEN  u32 LE   length of the postcard-encoded chunk
//!   CHUNK      bytes    postcard(Chunk)
//! ```
//!
//! The reader and writer are generic over [`Read`]/[`Write`] so the same byte
//! format can later be reused for non-file destinations (e.g. S3).

use std::io::{Read, Write};

use anyhow::{Context, bail};

use crate::models::Scope;
use crate::models::replication::{Chunk, Snapshot};

const MAGIC: [u8; 8] = *b"MYRCSNAP";
const FORMAT_VERSION: u8 = 1;
/// Cap the up-front capacity hint so a malformed `chunk_count` can't trigger a
/// huge allocation before any chunk bytes have been read.
const MAX_PREALLOC_CHUNKS: usize = 1024;

/// Self-describing metadata stored ahead of the chunk stream.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Header {
    pub scope: Scope,
    pub chunk_count: u64,
}

pub fn write_snapshot<W: Write>(
    w: &mut W,
    scope: &Scope,
    snapshot: &Snapshot,
) -> anyhow::Result<()> {
    w.write_all(&MAGIC)?;
    w.write_all(&[FORMAT_VERSION])?;

    let header = Header {
        scope: scope.clone(),
        chunk_count: snapshot.len() as u64,
    };
    let header_bytes =
        postcard::to_allocvec(&header).context("unable to encode snapshot header")?;
    write_frame(w, &header_bytes).context("unable to write snapshot header")?;

    for (i, chunk) in snapshot.iter().enumerate() {
        let bytes =
            postcard::to_allocvec(chunk).with_context(|| format!("unable to encode chunk {i}"))?;
        write_frame(w, &bytes).with_context(|| format!("unable to write chunk {i}"))?;
    }
    Ok(())
}

pub fn read_snapshot<R: Read>(r: &mut R) -> anyhow::Result<(Header, Snapshot)> {
    let mut magic = [0u8; MAGIC.len()];
    r.read_exact(&mut magic)
        .context("unable to read snapshot magic")?;
    if magic != MAGIC {
        bail!("not a swarm snapshot file (bad magic)");
    }

    let mut version = [0u8; 1];
    r.read_exact(&mut version)
        .context("unable to read snapshot version")?;
    if version[0] != FORMAT_VERSION {
        bail!(
            "unsupported snapshot format version {} (expected {FORMAT_VERSION})",
            version[0]
        );
    }

    let header_bytes = read_frame(r).context("unable to read snapshot header")?;
    let header: Header =
        postcard::from_bytes(&header_bytes).context("unable to decode snapshot header")?;

    let cap = usize::try_from(header.chunk_count.min(MAX_PREALLOC_CHUNKS as u64))
        .unwrap_or(MAX_PREALLOC_CHUNKS);
    let mut snapshot = Snapshot::with_capacity(cap);
    for i in 0..header.chunk_count {
        let bytes = read_frame(r).with_context(|| format!("unable to read chunk {i}"))?;
        let chunk: Chunk =
            postcard::from_bytes(&bytes).with_context(|| format!("unable to decode chunk {i}"))?;
        snapshot.push(chunk);
    }
    Ok((header, snapshot))
}

/// Write a `u32`-length-prefixed frame.
fn write_frame<W: Write>(w: &mut W, bytes: &[u8]) -> std::io::Result<()> {
    let len = u32::try_from(bytes.len()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "frame exceeds 4 GiB")
    })?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(bytes)
}

/// Read a `u32`-length-prefixed frame.
fn read_frame<R: Read>(r: &mut R) -> std::io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut bytes = vec![0u8; len];
    r.read_exact(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::replication::{SyncMarker, SyncMeta};

    fn sample_chunk(version: u64, entry: (&[u8], Option<&[u8]>)) -> Chunk {
        Chunk {
            id: (1, version, [7u8; 16]),
            meta: SyncMeta {
                parent: None,
                parent_epoch: None,
                marker: SyncMarker::Mutation,
                retention_period: None,
            },
            entries: vec![(entry.0.to_vec(), entry.1.map(<[u8]>::to_vec))],
        }
    }

    #[test]
    fn round_trips_snapshot_through_bytes() {
        let scope = Scope::new("ns", "db", "schema");
        let snapshot: Snapshot = vec![
            sample_chunk(10, (b"key-a", Some(b"value-a"))),
            sample_chunk(11, (b"key-b", None)),
        ];

        let mut buf = Vec::new();
        write_snapshot(&mut buf, &scope, &snapshot).unwrap();

        let (header, decoded) = read_snapshot(&mut buf.as_slice()).unwrap();

        assert_eq!(header.scope, scope);
        assert_eq!(header.chunk_count, 2);
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].id, (1, 10, [7u8; 16]));
        assert_eq!(
            decoded[0].entries,
            vec![(b"key-a".to_vec(), Some(b"value-a".to_vec()))]
        );
        assert_eq!(decoded[1].entries, vec![(b"key-b".to_vec(), None)]);
    }
}
