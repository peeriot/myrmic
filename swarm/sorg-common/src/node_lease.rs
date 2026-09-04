//! Db-backed node liveness leases (see the supervision spec). One row per
//! node, renewed periodically; observers track staleness via
//! [`crate::supervision::LeaseTracker`].

use core::str::FromStr;
use core::time::Duration;

use cell_protocol::{NODE_LEASE_TABLE, NodeLease, RuntimeId, node_lease_scope};
use db_client::v1::{
    Client as DbClient,
    models::{TxId, tb_delete, tb_insert, tb_list},
};
use tracing::warn;
use zenoh::Session;

use crate::{Result, custom_err};

/// `retention` bounds how long superseded renewal rows survive before GC
/// purges them; without it every renewal would accumulate forever. Must be
/// comfortably larger than the lease TTL so observers always judge live data.
pub async fn renew_lease(
    session: &Session,
    id: RuntimeId,
    lease: &NodeLease,
    retention: Duration,
) -> Result<()> {
    let lease = lease.clone();
    let db = DbClient::new(session);

    db.write_tx_in_with_retention(
        node_lease_scope(),
        Some(retention),
        async move |client, tx_id| Ok(do_renew(client, tx_id, id, &lease).await),
    )
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

pub async fn list_leases(session: &Session) -> Result<Vec<(RuntimeId, NodeLease)>> {
    let db = DbClient::new(session);

    db.read_tx_in(node_lease_scope(), async move |client, tx_id| {
        Ok(do_list(client, tx_id).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

/// Idempotent: deleting an already-gone lease row succeeds.
pub async fn delete_lease(session: &Session, id: RuntimeId) -> Result<()> {
    let db = DbClient::new(session);

    db.write_tx_in(node_lease_scope(), async move |client, tx_id| {
        Ok(do_delete(client, tx_id, id).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

pub async fn list_leases_in_tx(
    client: &DbClient,
    tx_id: TxId,
) -> Result<Vec<(RuntimeId, NodeLease)>> {
    do_list(client, tx_id).await
}

async fn do_renew(client: &DbClient, tx_id: TxId, id: RuntimeId, lease: &NodeLease) -> Result<()> {
    let value =
        postcard::to_allocvec(lease).map_err(|_| custom_err!("failed to serialize node lease"))?;

    client
        .send(tb_insert::Request {
            id: tx_id,
            op: tb_insert::Op {
                scope: node_lease_scope(),
                table: NODE_LEASE_TABLE.to_owned(),
                eid: Some(id.to_string().into_bytes()),
                value,
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send insert request: {}", err))?
        .map_err(|err| custom_err!("unable to renew lease: {}", err.message))?;

    Ok(())
}

async fn do_list(client: &DbClient, tx_id: TxId) -> Result<Vec<(RuntimeId, NodeLease)>> {
    let response = client
        .send(tb_list::Request {
            id: tx_id,
            op: tb_list::Op {
                scope: node_lease_scope(),
                table: NODE_LEASE_TABLE.to_owned(),
                cursor: None,
                limit: None,
                order: None,
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send list request: {}", err))?
        .map_err(|err| custom_err!("unable to list leases: {}", err.message))?;

    let mut leases = Vec::with_capacity(response.entities.len());
    for (eid, value) in response.entities {
        let id = core::str::from_utf8(&eid)
            .ok()
            .and_then(|s| RuntimeId::from_str(s).ok());
        let lease = postcard::from_bytes::<NodeLease>(&value).ok();
        match (id, lease) {
            (Some(id), Some(lease)) => leases.push((id, lease)),
            _ => warn!("skipping undecodable node-lease row"),
        }
    }
    Ok(leases)
}

async fn do_delete(client: &DbClient, tx_id: TxId, id: RuntimeId) -> Result<()> {
    let result = client
        .send(tb_delete::Request {
            id: tx_id,
            op: tb_delete::Op {
                scope: node_lease_scope(),
                table: NODE_LEASE_TABLE.to_owned(),
                eid: id.to_string().into_bytes(),
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send delete request: {}", err))?;

    if let Err(err) = result {
        warn!("lease delete for {id} reported: {}", err.message);
    }
    Ok(())
}
