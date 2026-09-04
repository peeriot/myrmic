use db_client::v1::{
    Client,
    models::{self, FieldValue, Scope, TxId, ts_publish},
};
use tracing::error;

pub async fn publish_measurement(
    db_client: Client,
    tx_id: TxId,
    scope: Scope,
    name: String,
    tags: Vec<(String, String)>,
    fields: Vec<(String, FieldValue)>,
    ts: u64,
) -> Result<(), &'static str> {
    let pub_request = ts_publish::Request {
        id: tx_id,
        op: ts_publish::Op {
            measurement: name,
            fields,
            timestamp: ts,
            tags,
            scope,
        },
    };

    match db_client.send(pub_request).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(_)) => {
            error!("failed to publish to ts store");
            Err("failed to write to db")
        }
        Err(_) => {
            error!("failed to write to db");
            Err("failed to write to db")
        }
    }
}

type FindResponse = models::ts_find::Response;

#[allow(clippy::too_many_arguments)]
pub async fn find_measurement(
    db_client: Client,
    tx_id: TxId,
    scope: Scope,
    name: String,
    limit: Option<usize>,
    start: Option<u64>,
    end: Option<u64>,
    order: Option<models::TsOrderBy>,
) -> Result<FindResponse, &'static str> {
    let find_request = models::ts_find::Request {
        id: tx_id,
        op: models::ts_find::Op {
            scope,
            measurement: name,
            limit,
            start,
            end,
            order,
        },
    };

    match db_client.send(find_request).await {
        Ok(Ok(resp)) => Ok(resp),
        Ok(Err(_)) => {
            error!("failed to find in store");
            Err("failed to find in store")
        }
        Err(_) => {
            error!("failed to read from db");
            Err("failed to read from db")
        }
    }
}
