//! The swarm's db, for a test that reads or writes it directly, e.g. a cell's data
//! ([`DbHandle`]). Every swarm has one ([`super::Swarm::get_db`]). A test without a
//! [`super::Swarm`] creates one on its own zenoh session with [`DbHandle::new`].

use db_client::Session;
use db_client::v1::Client;
use db_client::v1::models::Scope;

/// The swarm's db through a session that is part of the swarm; each call is a transaction of its
/// own.
///
/// The db keeps its data in scopes, and each cell's data is in a scope of its own,
/// `cell_protocol::scope_of_cell(sri)`. A cell's `State::new_const("n")` is the key `"n"` in that
/// scope, its value encoded with the cell's codec (postcard by default):
///
/// ```ignore
/// let n = swarm.get_db().get(cell_protocol::scope_of_cell(cell.sri()), "n").await?;
/// let n: Option<i32> = n.map(|bytes| postcard::from_bytes(&bytes)).transpose()?;
/// ```
pub struct DbHandle {
    client: Client,
}

/// Why a [`DbHandle`] operation failed.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// The transaction failed, e.g. because no runtime holding the scope could be reached.
    #[error("the db transaction failed: {0}")]
    Transaction(zenoh::Error),
    /// The db refused the operation.
    #[error("the db refused the operation: {0}")]
    Refused(String),
}

impl DbHandle {
    /// Create a handle on top of an existing zenoh session.
    pub fn new(session: &Session) -> Self {
        Self {
            client: Client::new(session),
        }
    }

    /// Write the raw bytes `value` under `key` in `scope`.
    pub async fn put(&self, scope: Scope, key: &str, value: Vec<u8>) -> Result<(), DbError> {
        let key = key.to_owned();
        self.client
            .write_tx_in(scope.clone(), async move |client, tx| {
                client
                    .send(db_client::v1::models::key_put::Request {
                        id: tx,
                        op: db_client::v1::models::key_put::Op { scope, key, value },
                    })
                    .await
            })
            .await
            .map_err(DbError::Transaction)?
            .map_err(|err| DbError::Refused(err.message))?;
        Ok(())
    }

    /// Write the UTF-8 string `value` under `key` in `scope`.
    pub async fn put_str(&self, scope: Scope, key: &str, value: &str) -> Result<(), DbError> {
        self.put(scope, key, value.as_bytes().to_vec()).await
    }

    /// The raw bytes under `key` in `scope`, or `None` if there are none.
    pub async fn get(&self, scope: Scope, key: &str) -> Result<Option<Vec<u8>>, DbError> {
        let key = key.to_owned();
        let response = self
            .client
            .read_tx_in(scope.clone(), async move |client, tx| {
                client
                    .send(db_client::v1::models::key_get::Request {
                        id: tx,
                        op: db_client::v1::models::key_get::Op { scope, key },
                    })
                    .await
            })
            .await
            .map_err(DbError::Transaction)?
            .map_err(|err| DbError::Refused(err.message))?;
        Ok(response.value)
    }

    /// Every `(entry id, value)` row of the table `table` in `scope`.
    pub async fn tb_list(
        &self,
        scope: Scope,
        table: &str,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, DbError> {
        let table = table.to_owned();
        self.client
            .read_tx_in(scope.clone(), async move |client, tx| {
                Ok(sorg_common::tb_list(client.clone(), tx, scope, table, None, None, None).await)
            })
            .await
            .map_err(DbError::Transaction)?
            .map_err(|err| DbError::Refused(err.to_owned()))
    }
}
