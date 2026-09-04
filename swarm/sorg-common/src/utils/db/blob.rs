use db_client::v1::Client;
use db_client::v1::models::{
    BlobId, BlobResponse, ChunkRange, Scope, TxId, blob_link, blob_move, blob_resolve, blob_store,
    blob_unlink, path_resolve, paths_list,
};

use tracing::error;

pub async fn blob_store(
    db_client: Client,
    tx_id: TxId,
    scope: Scope,
    blob: Vec<u8>,
) -> Result<BlobId, &'static str> {
    let store_request = blob_store::Request {
        id: tx_id,
        op: blob_store::Op { scope, blob },
    };

    match db_client.send(store_request).await {
        Ok(Ok(response)) => Ok(response.blob_id),
        Ok(Err(_)) => {
            error!("failed to store blob");
            Err("failed to store blob")
        }
        Err(_) => {
            error!("failed to write to db");
            Err("failed to write to db")
        }
    }
}

pub async fn blob_link(
    db_client: Client,
    tx_id: TxId,
    blob_id: BlobId,
    path: String,
) -> Result<(), &'static str> {
    let link_request = blob_link::Request {
        id: tx_id,
        op: blob_link::Op { blob_id, path },
    };

    match db_client.send(link_request).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(_)) => {
            error!("failed to link blob");
            Err("failed to link blob")
        }
        Err(_) => {
            error!("failed to write to db");
            Err("failed to write to db")
        }
    }
}

pub async fn blob_unlink(
    db_client: Client,
    tx_id: TxId,
    scope: Scope,
    path: String,
) -> Result<(), &'static str> {
    let unlink_request = blob_unlink::Request {
        id: tx_id,
        op: blob_unlink::Op { scope, path },
    };

    match db_client.send(unlink_request).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(_)) => {
            error!("failed to unlink blob path");
            Err("failed to unlink blob path")
        }
        Err(_) => {
            error!("failed to write to db");
            Err("failed to write to db")
        }
    }
}

pub async fn blob_move(
    db_client: Client,
    tx_id: TxId,
    scope: Scope,
    old_path: String,
    new_path: String,
) -> Result<(), &'static str> {
    let move_request = blob_move::Request {
        id: tx_id,
        op: blob_move::Op {
            scope,
            old_path,
            new_path,
        },
    };

    match db_client.send(move_request).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(_)) => {
            error!("failed to move blob path");
            Err("failed to move blob path")
        }
        Err(_) => {
            error!("failed to write to db");
            Err("failed to write to db")
        }
    }
}

pub async fn blob_resolve(
    db_client: Client,
    tx_id: TxId,
    blob_id: BlobId,
    range: Option<ChunkRange>,
) -> Result<Option<BlobResponse>, &'static str> {
    let resolve_request = blob_resolve::Request {
        id: tx_id,
        op: blob_resolve::Op { blob_id, range },
    };

    match db_client.send(resolve_request).await {
        Ok(Ok(resp)) => Ok(resp.blob),
        Ok(Err(_)) => {
            error!("failed to resolve blob");
            Err("failed to resolve blob")
        }
        Err(_) => {
            error!("failed to send read to db");
            Err("failed to send read to db")
        }
    }
}

pub async fn path_resolve(
    db_client: Client,
    tx_id: TxId,
    scope: Scope,
    path: String,
    range: Option<ChunkRange>,
) -> Result<Option<BlobResponse>, &'static str> {
    let resolve_request = path_resolve::Request {
        id: tx_id,
        op: path_resolve::Op { scope, path, range },
    };

    match db_client.send(resolve_request).await {
        Ok(Ok(resp)) => Ok(resp.blob),
        Ok(Err(_)) => {
            error!("failed to resolve blob path");
            Err("failed to resolve blob path")
        }
        Err(_) => {
            error!("failed to send read to db");
            Err("failed to send read to db")
        }
    }
}

pub async fn paths_list(
    db_client: Client,
    tx_id: TxId,
    scope: Scope,
    limit: Option<usize>,
) -> Result<Vec<String>, &'static str> {
    let list_request = paths_list::Request {
        id: tx_id,
        op: paths_list::Op { scope, limit },
    };

    match db_client.send(list_request).await {
        Ok(Ok(resp)) => Ok(resp.paths),
        Ok(Err(_)) => {
            error!("failed to list blob paths");
            Err("failed to list blob paths")
        }
        Err(_) => {
            error!("failed to send read to db");
            Err("failed to send read to db")
        }
    }
}
