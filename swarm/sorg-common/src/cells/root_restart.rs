//! Root restart registry: the dedicated, orchestrator-owned table that stores
//! everything needed to auto-restart a root — the original [`CellDeployment`],
//! replayed verbatim (with a fresh generation) after a qualifying death. Keyed
//! by SRI. A row exists only while the root is restartable; every terminal
//! path erases it and a reconciliation sweep drops any that leak, so specs
//! never outlive the root they belong to.

use cell_protocol::{ROOT_RESTART_TABLE, Sri, root_restart_scope};
use db_client::v1::{
    Client as DbClient,
    models::{TxId, tb_delete, tb_get, tb_insert, tb_list},
};
use zenoh::Session;

use crate::{CellDeployment, Result, custom_err};

/// Writes (or overwrites) a root's restart spec in its own transaction.
pub async fn write_spec(session: &Session, deployment: &CellDeployment) -> Result<()> {
    let deployment = deployment.clone();
    let db = DbClient::new(session);
    db.write_tx_in(root_restart_scope(), async move |client, tx_id| {
        Ok(do_write(client, tx_id, &deployment).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

/// Writes a root's restart spec using an externally-managed transaction, so it
/// commits atomically with the deploy that created the root.
pub async fn write_spec_in_tx(
    session: &Session,
    tx_id: TxId,
    deployment: &CellDeployment,
) -> Result<()> {
    let db = DbClient::new(session);
    do_write(&db, tx_id, deployment).await
}

/// Reads a root's restart spec, if one exists.
pub async fn get_spec(session: &Session, sri: &Sri) -> Result<Option<CellDeployment>> {
    let sri = *sri;
    let db = DbClient::new(session);
    db.read_tx_in(root_restart_scope(), async move |client, tx_id| {
        Ok(do_get(client, tx_id, &sri).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

/// Lists all root restart specs.
pub async fn list_specs(session: &Session) -> Result<Vec<CellDeployment>> {
    let db = DbClient::new(session);
    db.read_tx_in(root_restart_scope(), async move |client, tx_id| {
        Ok(do_list(client, tx_id).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

/// Erases a root's restart spec if present, returning whether one was removed.
pub async fn erase_spec(session: &Session, sri: &Sri) -> Result<bool> {
    let sri = *sri;
    let db = DbClient::new(session);
    db.write_tx_in(root_restart_scope(), async move |client, tx_id| {
        Ok(do_erase(client, tx_id, &sri).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

/// Erases a root's restart spec using an externally-managed transaction, so it
/// commits atomically with the undeploy that removed the root.
pub async fn erase_spec_in_tx(session: &Session, tx_id: TxId, sri: &Sri) -> Result<bool> {
    let db = DbClient::new(session);
    do_erase(&db, tx_id, sri).await
}

async fn do_write(client: &DbClient, tx_id: TxId, deployment: &CellDeployment) -> Result<()> {
    let value = postcard::to_allocvec(deployment)
        .map_err(|_| custom_err!("failed to serialize root restart spec"))?;
    client
        .send(tb_insert::Request {
            id: tx_id,
            op: tb_insert::Op {
                scope: root_restart_scope(),
                table: ROOT_RESTART_TABLE.to_owned(),
                eid: Some(deployment.sri.to_string().into_bytes()),
                value,
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send insert request: {}", err))?
        .map_err(|err| custom_err!("unable to insert root restart spec: {}", err.message))?;
    Ok(())
}

async fn do_get(client: &DbClient, tx_id: TxId, sri: &Sri) -> Result<Option<CellDeployment>> {
    let response = client
        .send(tb_get::Request {
            id: tx_id,
            op: tb_get::Op {
                scope: root_restart_scope(),
                table: ROOT_RESTART_TABLE.to_owned(),
                eid: sri.to_string().into_bytes(),
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send get request: {}", err))?
        .map_err(|err| custom_err!("unable to get root restart spec: {}", err.message))?;

    match response.value {
        Some(bytes) => {
            let record = postcard::from_bytes::<CellDeployment>(&bytes)
                .map_err(|_| custom_err!("failed to deserialize root restart spec"))?;
            Ok(Some(record))
        }
        None => Ok(None),
    }
}

async fn do_list(client: &DbClient, tx_id: TxId) -> Result<Vec<CellDeployment>> {
    let response = client
        .send(tb_list::Request {
            id: tx_id,
            op: tb_list::Op {
                scope: root_restart_scope(),
                table: ROOT_RESTART_TABLE.to_owned(),
                cursor: None,
                limit: None,
                order: None,
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send list request: {}", err))?
        .map_err(|err| custom_err!("unable to list root restart specs: {}", err.message))?;

    let mut specs = Vec::with_capacity(response.entities.len());
    for (_id, value_bytes) in response.entities {
        let record = postcard::from_bytes::<CellDeployment>(&value_bytes)
            .map_err(|_| custom_err!("failed to deserialize root restart spec"))?;
        specs.push(record);
    }
    Ok(specs)
}

async fn do_erase(client: &DbClient, tx_id: TxId, sri: &Sri) -> Result<bool> {
    if do_get(client, tx_id, sri).await?.is_none() {
        return Ok(false);
    }
    client
        .send(tb_delete::Request {
            id: tx_id,
            op: tb_delete::Op {
                scope: root_restart_scope(),
                table: ROOT_RESTART_TABLE.to_owned(),
                eid: sri.to_string().into_bytes(),
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send delete request: {}", err))?
        .map_err(|err| custom_err!("unable to delete root restart spec: {}", err.message))?;
    Ok(true)
}
