//! A path-keyed blob store, owning the scratch buffers the raw
//! [`crate::db::blob`] functions need.

use myrmic_common::db::{BlobPath, ChunkRange, Scope};

use crate::db::blob;
use crate::{Result, String, Vec};

/// Scratch space for a request; scope and path are the only things in it.
const REQUEST_BUFFER: usize = 1024;

/// Files reachable by path within one blob [`Scope`].
///
/// This is what the gateway serves static assets out of: write the files at
/// init, then mount the scope (see [`crate::gateway`]).
///
/// ```
/// # fn demo(md: myrmic_sdk::Metadata) -> myrmic_sdk::Result<()> {
/// let assets = myrmic_sdk::gateway::assets(md.id);
/// assets.put("/index.html", b"<h1>hello</h1>")?;
/// # Ok(())
/// # }
/// ```
pub struct BlobStore {
    scope: Scope,
}

impl BlobStore {
    /// A store over an explicit scope.
    #[must_use]
    pub fn new(scope: Scope) -> Self {
        Self { scope }
    }

    /// The scope this store reads and writes.
    #[must_use]
    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    /// Stores every `(path, bytes)` pair via [`Self::put`].
    pub fn upload<'a, I, S, B>(&self, iter: I) -> Result
    where
        I: IntoIterator<Item = &'a (S, B)>,
        S: AsRef<str> + 'a,
        B: AsRef<[u8]> + 'a,
    {
        for (path, bytes) in iter.into_iter() {
            self.put(path.as_ref(), bytes.as_ref())?;
        }
        Ok(())
    }

    /// Stores `bytes` and links them at `path`, replacing whatever was there.
    ///
    /// `bytes` is handed to the host by reference, so static assets never hit
    /// the guest heap.
    pub fn put(&self, path: &str, bytes: &[u8]) -> Result {
        let mut req = [0u8; REQUEST_BUFFER];
        let mut rsp = [0u8; REQUEST_BUFFER];

        let blob_id = blob::blob_store(self.scope.clone(), bytes, &mut req, &mut rsp)
            .map_err(|_| "BlobStore::put (store)")?;

        blob::blob_link(blob_id, normalize(path), &mut req).map_err(|_| "BlobStore::put (link)")
    }

    /// Reads the file at `path`, or `None` if nothing is linked there.
    ///
    /// Resolves the size first and then fetches exactly that many bytes, so
    /// there is no buffer to guess at.
    pub fn get(&self, path: &str) -> Result<Option<Vec<u8>>> {
        let path = normalize(path);

        let Some(size) = self.size_of(&path)? else {
            return Ok(None);
        };

        let mut req = [0u8; REQUEST_BUFFER];
        // Room for the postcard framing around the bytes themselves.
        let mut rsp = crate::vec![0u8; size as usize + REQUEST_BUFFER];

        let response = blob::path_resolve(self.scope.clone(), path, None, &mut req, &mut rsp)
            .map_err(|_| "BlobStore::get")?;
        Ok(response.map(|response| response.blob))
    }

    /// Size in bytes of the file at `path`, or `None` if nothing is linked there.
    ///
    /// A zero-length range asks the host for metadata only, so this does not
    /// transfer the blob.
    pub fn size_of(&self, path: &str) -> Result<Option<u64>> {
        let mut req = [0u8; REQUEST_BUFFER];
        let mut rsp = [0u8; REQUEST_BUFFER];

        let probe = ChunkRange {
            offset: 0,
            length: 0,
        };
        let response = blob::path_resolve(
            self.scope.clone(),
            normalize(path),
            Some(probe),
            &mut req,
            &mut rsp,
        )
        .map_err(|_| "BlobStore::size_of")?;

        Ok(response.map(|response| response.total_len))
    }

    /// Reads `length` bytes of the file at `path`, starting at `offset` — for
    /// files too large to hold in memory whole.
    pub fn get_range(&self, path: &str, offset: u64, length: u64) -> Result<Option<Vec<u8>>> {
        let mut req = [0u8; REQUEST_BUFFER];
        let mut rsp = crate::vec![0u8; length as usize + REQUEST_BUFFER];

        let range = ChunkRange { offset, length };
        let response = blob::path_resolve(
            self.scope.clone(),
            normalize(path),
            Some(range),
            &mut req,
            &mut rsp,
        )
        .map_err(|_| "BlobStore::get_range")?;

        Ok(response.map(|response| response.blob))
    }

    /// Unlinks `path`. The underlying blob survives while other paths use it.
    pub fn delete(&self, path: &str) -> Result<()> {
        let mut req = [0u8; REQUEST_BUFFER];
        blob::blob_unlink(self.scope.clone(), normalize(path), &mut req)
            .map_err(|_| "BlobStore::delete")
    }

    /// Re-points `from` at `to`, leaving the blob untouched.
    pub fn rename(&self, from: &str, to: &str) -> Result<()> {
        let mut req = [0u8; REQUEST_BUFFER];
        blob::blob_move(self.scope.clone(), normalize(from), normalize(to), &mut req)
            .map_err(|_| "BlobStore::rename")
    }

    /// Every path in this store.
    pub fn list(&self) -> Result<Vec<BlobPath>> {
        let mut req = [0u8; REQUEST_BUFFER];
        let mut rsp = crate::vec![0u8; 8192];
        blob::paths_list(self.scope.clone(), None, &mut req, &mut rsp)
            .map_err(|_| "BlobStore::list")
    }
}

/// Paths are stored with exactly one leading slash, so `index.html` and
/// `/index.html` name the same file.
fn normalize(path: &str) -> String {
    let trimmed = path.trim_start_matches('/');
    let mut normalized = String::with_capacity(trimmed.len() + 1);
    normalized.push('/');
    normalized.push_str(trimmed);
    normalized
}
