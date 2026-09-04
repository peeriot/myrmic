use db_client::v1::{
    Client,
    models::{self, Scope, TxId, sem_select, sem_update},
};
use tracing::error;

pub async fn sem_update(
    db_client: Client,
    tx_id: TxId,
    scope: Scope,
    query: String,
    base_iri: Option<String>,
) -> Result<(), &'static str> {
    let update_request = sem_update::Request {
        id: tx_id,
        op: sem_update::Op {
            scope,
            query,
            base_iri,
        },
    };

    match db_client.send(update_request).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(_)) => {
            error!("failed the sem update");
            Err("failed the sem update")
        }
        Err(_) => {
            error!("failed to write to db");
            Err("failed to write to db")
        }
    }
}

pub async fn sem_select(
    db_client: Client,
    tx_id: TxId,
    scope: Scope,
    query: String,
    base_iri: Option<String>,
    limit: Option<usize>,
    skip: Option<usize>,
) -> Result<models::sem_select::Response, &'static str> {
    let select_request = sem_select::Request {
        id: tx_id,
        op: sem_select::Op {
            scope,
            query,
            base_iri,
            limit,
            skip,
        },
    };

    match db_client.send(select_request).await {
        Ok(Ok(resp)) => Ok(resp),
        Ok(Err(_)) => {
            error!("failed sem select");
            Err("failed sem select")
        }
        Err(_) => {
            error!("failed to send read to");
            Err("failed to send read to")
        }
    }
}
