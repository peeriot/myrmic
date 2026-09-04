use cell_protocol::{CellInstance, INSTANCE_REGISTRY_TABLE, Sri, instance_registry_scope};
use db_client::v1::{
    Client as DbClient,
    models::{TxId, tb_delete, tb_get, tb_insert, tb_list},
};
use zenoh::Session;

use super::placement;
use crate::{Result, bail, custom_err};

pub async fn erase_instance(session: &Session, sri: &Sri) -> Result<()> {
    let sri = *sri;
    let db = DbClient::new(session);

    db.write_tx_in(instance_registry_scope(), async move |client, tx_id| {
        Ok(do_erase(client, tx_id, &sri).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

/// Erases the instance row if it exists, returning whether one was deleted.
/// Still refuses while the cell is deployed. For callers that may race with
/// undeploy's own erase, where an absent row means the work is already done.
pub async fn erase_instance_if_present(session: &Session, sri: &Sri) -> Result<bool> {
    let sri = *sri;
    let db = DbClient::new(session);

    db.write_tx_in(instance_registry_scope(), async move |client, tx_id| {
        Ok(do_erase_if_present(client, tx_id, &sri).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

pub async fn list_instances(session: &Session) -> Result<Vec<CellInstance>> {
    let db = DbClient::new(session);

    db.read_tx_in(instance_registry_scope(), async move |client, tx_id| {
        Ok(do_list(client, tx_id).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

/// Inserts a registry entry using an externally-managed transaction.
/// The caller is responsible for committing the transaction. The row is
/// keyed by `record.sri`.
pub async fn insert_registry_entry_in_tx(
    session: &Session,
    tx_id: TxId,
    record: &CellInstance,
) -> Result<()> {
    let db = DbClient::new(session);
    do_insert_registry_entry(&db, tx_id, record).await
}

/// Inserts a registry entry in its own write transaction.
pub async fn insert_registry_entry(session: &Session, record: &CellInstance) -> Result<()> {
    let record = record.clone();
    let db = DbClient::new(session);

    db.write_tx_in(instance_registry_scope(), async move |client, tx_id| {
        Ok(do_insert_registry_entry(client, tx_id, &record).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

pub async fn get_instance(session: &Session, sri: &Sri) -> Result<Option<CellInstance>> {
    let sri = *sri;
    let db = DbClient::new(session);

    db.read_tx_in(instance_registry_scope(), async move |client, tx_id| {
        Ok(do_get(client, tx_id, &sri).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

async fn do_erase(client: &DbClient, tx_id: TxId, sri: &Sri) -> Result<()> {
    if !do_erase_if_present(client, tx_id, sri).await? {
        bail!("instance '{}' not found", sri);
    }
    Ok(())
}

async fn do_erase_if_present(client: &DbClient, tx_id: TxId, sri: &Sri) -> Result<bool> {
    let existing = do_get_record(client, tx_id, sri).await?;
    if existing.is_none() {
        return Ok(false);
    }
    if placement::placement_exists_in_tx(client, tx_id, sri).await? {
        bail!(
            "cannot erase instance '{}': cell is currently deployed",
            sri
        );
    }
    do_delete_registry_entry(client, tx_id, sri).await?;
    Ok(true)
}

pub(crate) async fn do_list(client: &DbClient, tx_id: TxId) -> Result<Vec<CellInstance>> {
    let response = client
        .send(tb_list::Request {
            id: tx_id,
            op: tb_list::Op {
                scope: instance_registry_scope(),
                table: INSTANCE_REGISTRY_TABLE.to_owned(),
                cursor: None,
                limit: None,
                order: None,
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send list request: {}", err))?
        .map_err(|err| custom_err!("unable to list instances: {}", err.message))?;

    let mut instances = Vec::with_capacity(response.entities.len());
    for (_id, value_bytes) in response.entities {
        let record = postcard::from_bytes::<CellInstance>(&value_bytes)
            .map_err(|_| custom_err!("failed to deserialize instance entry"))?;
        instances.push(record);
    }
    Ok(instances)
}

pub(crate) async fn do_get(
    client: &DbClient,
    tx_id: TxId,
    sri: &Sri,
) -> Result<Option<CellInstance>> {
    do_get_record(client, tx_id, sri).await
}

async fn do_get_record(client: &DbClient, tx_id: TxId, sri: &Sri) -> Result<Option<CellInstance>> {
    let response = client
        .send(tb_get::Request {
            id: tx_id,
            op: tb_get::Op {
                scope: instance_registry_scope(),
                table: INSTANCE_REGISTRY_TABLE.to_owned(),
                eid: sri.to_string().into_bytes(),
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send get request: {}", err))?
        .map_err(|err| custom_err!("unable to get instance: {}", err.message))?;

    match response.value {
        Some(bytes) => {
            let record = postcard::from_bytes::<CellInstance>(&bytes)
                .map_err(|_| custom_err!("failed to deserialize instance entry"))?;
            Ok(Some(record))
        }
        None => Ok(None),
    }
}

async fn do_insert_registry_entry(
    client: &DbClient,
    tx_id: TxId,
    record: &CellInstance,
) -> Result<()> {
    let value = postcard::to_allocvec(record)
        .map_err(|_| custom_err!("failed to serialize instance entry"))?;

    client
        .send(tb_insert::Request {
            id: tx_id,
            op: tb_insert::Op {
                scope: instance_registry_scope(),
                table: INSTANCE_REGISTRY_TABLE.to_owned(),
                eid: Some(record.sri.to_string().into_bytes()),
                value,
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send insert request: {}", err))?
        .map_err(|err| custom_err!("unable to insert instance: {}", err.message))?;

    Ok(())
}

async fn do_delete_registry_entry(client: &DbClient, tx_id: TxId, sri: &Sri) -> Result<()> {
    client
        .send(tb_delete::Request {
            id: tx_id,
            op: tb_delete::Op {
                scope: instance_registry_scope(),
                table: INSTANCE_REGISTRY_TABLE.to_owned(),
                eid: sri.to_string().into_bytes(),
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send delete request: {}", err))?
        .map_err(|err| custom_err!("unable to delete instance: {}", err.message))?;

    Ok(())
}
