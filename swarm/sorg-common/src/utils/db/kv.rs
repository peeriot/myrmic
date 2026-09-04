use db_client::v1::{
    Client,
    models::{Scope, TxId, key_delete, key_get, key_prefix, key_put},
};
use tracing::error;

pub async fn key_put(
    db_client: Client,
    tx_id: TxId,
    scope: Scope,
    key: String,
    value: Vec<u8>,
) -> Result<(), &'static str> {
    let put_request = key_put::Request {
        id: tx_id,
        op: key_put::Op { key, scope, value },
    };

    match db_client.send(put_request).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(_)) => Err("failed to put key"),
        Err(_) => {
            error!("failed to write to db");
            Err("failed to write to db")
        }
    }
}

pub async fn key_get(
    db_client: Client,
    tx_id: TxId,
    scope: Scope,
    key: String,
) -> Result<Option<Vec<u8>>, &'static str> {
    let find_request = key_get::Request {
        id: tx_id,
        op: key_get::Op { scope, key },
    };

    match db_client.send(find_request).await {
        Ok(Ok(resp)) => Ok(resp.value),
        Ok(Err(_)) => {
            error!("failed to get key from store");
            Err("failed to get key from store")
        }
        Err(_) => {
            error!("failed to send read to");
            Err("failed to send read to")
        }
    }
}

pub async fn key_prefix(
    db_client: Client,
    tx_id: TxId,
    scope: Scope,
    prefix: String,
) -> Result<Vec<String>, &'static str> {
    let prefix_request = key_prefix::Request {
        id: tx_id,
        op: key_prefix::Op { scope, prefix },
    };

    match db_client.send(prefix_request).await {
        Ok(Ok(resp)) => Ok(resp.keys),
        Ok(Err(_)) => {
            error!("failed to get keys from store");
            Err("failed to get keys from store")
        }
        Err(_) => {
            error!("failed to send read to");
            Err("failed to send read to")
        }
    }
}

pub async fn key_delete(
    db_client: Client,
    tx_id: TxId,
    scope: Scope,
    key: String,
) -> Result<(), &'static str> {
    let put_request = key_delete::Request {
        id: tx_id,
        op: key_delete::Op { key, scope },
    };

    match db_client.send(put_request).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(_)) => {
            error!("failed to delete key");
            Err("failed to delete key")
        }
        Err(_) => {
            error!("failed to write to db");
            Err("failed to write to db")
        }
    }
}
