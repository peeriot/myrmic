//! The swarm's db, for a test that reads or writes it directly, e.g. a cell's data
//! ([`DbHandle`]). A swarm has one ([`super::Swarm::get_db`]); [`DbHandle::new`] makes one on any
//! session, for a test without a [`super::Swarm`].

use db_client::Session;
use db_client::v1::Client;
use db_client::v1::models::Scope;

/// The swarm's db through a session that is part of the swarm; each call is a transaction of its
/// own.
pub struct DbHandle {
    client: Client,
}

impl DbHandle {
    /// Create a handle on top of an existing zenoh session.
    pub fn new(session: &Session) -> Self {
        Self {
            client: Client::new(session),
        }
    }

    /// Insert raw bytes at the given scope + key.
    pub async fn put(&self, scope: Scope, key: &str, value: Vec<u8>) {
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
            .expect("db key_put failed")
            .expect("db key_put returned error");
    }

    /// Insert a UTF-8 string value
    pub async fn put_str(&self, scope: Scope, key: &str, value: &str) {
        self.put(scope, key, value.as_bytes().to_vec()).await;
    }

    /// Read raw bytes from the given scope + key.
    pub async fn get(&self, scope: Scope, key: &str) -> Option<Vec<u8>> {
        let key = key.to_owned();
        self.client
            .read_tx_in(scope.clone(), async move |client, tx| {
                client
                    .send(db_client::v1::models::key_get::Request {
                        id: tx,
                        op: db_client::v1::models::key_get::Op { scope, key },
                    })
                    .await
            })
            .await
            .expect("db key_get failed")
            .expect("db key_get returned error")
            .value
    }

    /// List every `(entry-id, value)` row in a table.
    pub async fn tb_list(&self, scope: Scope, table: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
        let table = table.to_owned();
        self.client
            .read_tx_in(scope.clone(), async move |client, tx| {
                Ok(sorg_common::tb_list(client.clone(), tx, scope, table, None, None, None).await)
            })
            .await
            .expect("db tb_list failed")
            .expect("db tb_list returned error")
    }
}
