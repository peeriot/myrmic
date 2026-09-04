use sorg_common::custom_err;
use zenoh::Session;

use crate::Result;

use db_client::v1::models;

/// Used to interact with the file store
#[derive(Debug, Clone)]
pub struct Client {
    client: db_client::v1::Client,
    scope: models::Scope,
}

impl Client {
    #[must_use]
    pub fn new(session: &Session) -> Self {
        Self::new_with_scope(session, Default::default())
    }

    #[must_use]
    pub fn new_with_scope(session: &Session, scope: models::Scope) -> Self {
        let client = db_client::v1::Client::new(session);
        Self { client, scope }
    }

    /// Checks whether the client can reach a filestore plugin
    pub async fn is_fs_present(&self) -> bool {
        self.client.ping().await.is_ok_and(|r| r.is_ok())
    }

    /// Checks whether the given file is present in the filestore
    /// Returns an error if the filestore cannot be reached
    pub async fn is_file_present(&self, file_path: &str) -> Result<bool> {
        let file = self.get_file(file_path).await?;
        Ok(file.is_some())
    }

    /// Returns the bytes -- as `Some(Vec<u8>)` -- of the specified file if it is present; returns None otherwise
    /// Returns an error if the filestore cannot be reached
    pub async fn get_file(&self, file_path: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .client
            .read_tx_in(self.scope.clone(), {
                let scope = self.scope.clone();
                let mut path = String::from(file_path);

                if !path.starts_with('/') {
                    path.insert(0, '/');
                }

                async move |client, tx| {
                    client
                        .send(models::path_resolve::Request {
                            id: tx,
                            op: models::path_resolve::Op {
                                scope,
                                path,
                                range: None,
                            },
                        })
                        .await
                }
            })
            .await
            .map_err(|err| custom_err!("unable to query fs: {}", err))?
            .map_err(|err| custom_err!("{}", err.message))?
            .blob
            .map(|blob| blob.blob))
    }

    pub async fn store_file_if_absent(&self, path: &str, local: &std::path::Path) -> Result<()> {
        if self.is_file_present(path).await? {
            return Ok(());
        }

        let bytes = tokio::fs::read(local)
            .await
            .map_err(|err| custom_err!("unable to read file {}: {}", local.display(), err))?;
        self.store_file(path, bytes).await?;
        Ok(())
    }

    pub async fn store_if_absent(&self, path: &str, bytes: &[u8]) -> Result<()> {
        if self.is_file_present(path).await? {
            return Ok(());
        }

        self.store_file(path, bytes.to_vec()).await?;
        Ok(())
    }

    /// Stores the provided bytes under the provided path (specified relatively to the root dir of the filestore).
    pub async fn store_file(&self, file_path: &str, bytes: Vec<u8>) -> Result<()> {
        self.client
            .write_tx_in(self.scope.clone(), {
                let scope = self.scope.clone();
                let mut path = String::from(file_path);

                if !path.starts_with('/') {
                    path.insert(0, '/');
                }

                async move |client, tx| {
                    let blob_id = client
                        .send(models::blob_store::Request {
                            id: tx,
                            op: models::blob_store::Op { scope, blob: bytes },
                        })
                        .await?
                        .map_err(|err| custom_err!("unable to store blob: {}", err.message))?
                        .blob_id;

                    client
                        .send(models::blob_link::Request {
                            id: tx,
                            op: models::blob_link::Op {
                                blob_id: blob_id.clone(),
                                path,
                            },
                        })
                        .await?
                        .map_err(|err| custom_err!("unable to link: {}", err.message))?;

                    Ok(blob_id)
                }
            })
            .await
            .map_err(|err| custom_err!("unable to query fs: {}", err))?;

        Ok(())
    }

    /// Stores the provided bytes and returns the hex-encoded content hash assigned by the datalayer.
    pub async fn store_file_hashed(&self, file_path: &str, bytes: Vec<u8>) -> Result<String> {
        let blob_id = self
            .client
            .write_tx_in(self.scope.clone(), {
                let scope = self.scope.clone();
                let mut path = String::from(file_path);

                if !path.starts_with('/') {
                    path.insert(0, '/');
                }

                async move |client, tx| {
                    let blob_id = client
                        .send(models::blob_store::Request {
                            id: tx,
                            op: models::blob_store::Op { scope, blob: bytes },
                        })
                        .await?
                        .map_err(|err| custom_err!("unable to store blob: {}", err.message))?
                        .blob_id;

                    client
                        .send(models::blob_link::Request {
                            id: tx,
                            op: models::blob_link::Op {
                                blob_id: blob_id.clone(),
                                path,
                            },
                        })
                        .await?
                        .map_err(|err| custom_err!("unable to link: {}", err.message))?;

                    Ok(blob_id)
                }
            })
            .await
            .map_err(|err| custom_err!("unable to query fs: {}", err))?;

        Ok(blob_id.hash.to_hex())
    }

    /// Retrieves a file by its hex-encoded content hash.
    pub async fn get_file_by_hash(&self, hash: &str) -> Result<Option<Vec<u8>>> {
        let blob_hash =
            models::BlobHash::from_hex(hash).ok_or_else(|| custom_err!("invalid content hash"))?;

        let blob_id = models::BlobId {
            scope: self.scope.clone(),
            hash: blob_hash,
        };

        Ok(self
            .client
            .read_tx_in(self.scope.clone(), {
                async move |client, tx| {
                    client
                        .send(models::blob_resolve::Request {
                            id: tx,
                            op: models::blob_resolve::Op {
                                blob_id,
                                range: None,
                            },
                        })
                        .await
                }
            })
            .await
            .map_err(|err| custom_err!("unable to query fs: {}", err))?
            .map_err(|err| custom_err!("{}", err.message))?
            .blob
            .map(|blob| blob.blob))
    }

    /// Deletes the specified file. Errors out if no such file was in the filestore
    pub async fn delete_file(&self, file_path: &str) -> Result<()> {
        self.client
            .write_tx_in(self.scope.clone(), {
                let scope = self.scope.clone();
                let mut path = String::from(file_path);

                if !path.starts_with('/') {
                    path.insert(0, '/');
                }

                async move |client, tx| {
                    client
                        .send(models::blob_unlink::Request {
                            id: tx,
                            op: models::blob_unlink::Op { scope, path },
                        })
                        .await
                }
            })
            .await
            .map_err(|err| custom_err!("unable to query fs: {}", err))?
            .map_err(|err| custom_err!("{}", err.message))?;

        Ok(())
    }
}
