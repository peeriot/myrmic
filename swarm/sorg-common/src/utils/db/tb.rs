use db_client::v1::Client;
use db_client::v1::models::{
    Cursor, Scope, TbOrderBy, TxId, tb_count, tb_delete, tb_get, tb_insert, tb_list,
};

use tracing::error;

pub async fn tb_insert(
    db_client: Client,
    tx_id: TxId,
    scope: Scope,
    table: String,
    eid: Option<Vec<u8>>,
    value: Vec<u8>,
) -> Result<Vec<u8>, &'static str> {
    let insert_request = tb_insert::Request {
        id: tx_id,
        op: tb_insert::Op {
            scope,
            table,
            eid,
            value,
        },
    };

    match db_client.send(insert_request).await {
        Ok(Ok(response)) => Ok(response.eid),
        Ok(Err(_)) => {
            error!("failed to insert entity");
            Err("failed to insert entity")
        }
        Err(_) => {
            error!("failed to write to db");
            Err("failed to write to db")
        }
    }
}

pub async fn tb_count(
    db_client: Client,
    tx_id: TxId,
    scope: Scope,
    table: String,
) -> Result<usize, &'static str> {
    let count_request = tb_count::Request {
        id: tx_id,
        op: tb_count::Op { scope, table },
    };

    match db_client.send(count_request).await {
        Ok(Ok(resp)) => Ok(resp.count),
        Ok(Err(_)) => {
            error!("failed to count entities in store");
            Err("failed to count entities in store")
        }
        Err(_) => {
            error!("failed to send read to");
            Err("failed to send read to")
        }
    }
}

pub async fn tb_get(
    db_client: Client,
    tx_id: TxId,
    scope: Scope,
    table: String,
    eid: Vec<u8>,
) -> Result<Option<Vec<u8>>, &'static str> {
    let get_request = tb_get::Request {
        id: tx_id,
        op: tb_get::Op { scope, table, eid },
    };

    match db_client.send(get_request).await {
        Ok(Ok(resp)) => Ok(resp.value),
        Ok(Err(_)) => {
            error!("failed to get entity from store");
            Err("failed to get entity from store")
        }
        Err(_) => {
            error!("failed to send read to");
            Err("failed to send read to")
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn tb_list(
    db_client: Client,
    tx_id: TxId,
    scope: Scope,
    table: String,
    cursor: Option<Cursor>,
    limit: Option<usize>,
    order: Option<TbOrderBy>,
) -> Result<Vec<(Vec<u8>, Vec<u8>)>, &'static str> {
    let list_request = tb_list::Request {
        id: tx_id,
        op: tb_list::Op {
            scope,
            table,
            cursor,
            limit,
            order,
        },
    };

    match db_client.send(list_request).await {
        Ok(Ok(resp)) => Ok(resp.entities),
        Ok(Err(_)) => {
            error!("failed to list entities in store");
            Err("failed to list entities in store")
        }
        Err(_) => {
            error!("failed to send read to");
            Err("failed to send read to")
        }
    }
}

pub async fn tb_delete(
    db_client: Client,
    tx_id: TxId,
    scope: Scope,
    table: String,
    eid: Vec<u8>,
) -> Result<(), &'static str> {
    let delete_request = tb_delete::Request {
        id: tx_id,
        op: tb_delete::Op { scope, table, eid },
    };

    match db_client.send(delete_request).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(_)) => {
            error!("failed to delete entity");
            Err("failed to delete entity")
        }
        Err(_) => {
            error!("failed to write to db");
            Err("failed to write to db")
        }
    }
}
