use cell_protocol::{Gen, PLACEMENT_TABLE, PlacementEntry, Sri, placement_scope};
use db_client::v1::{
    Client as DbClient,
    models::{TxId, tb_delete, tb_get, tb_insert, tb_list},
};
use zenoh::Session;

use super::fence::{self, FenceOutcome};
use crate::{Result, custom_err};

/// Lists all placements within an existing read transaction. Use this inside
/// the placement OCC loop so the occupancy snapshot is consistent with the
/// exec list.
pub async fn list_placements_in_tx(db: &DbClient, tx_id: TxId) -> Result<Vec<PlacementEntry>> {
    let response = db
        .send(tb_list::Request {
            id: tx_id,
            op: tb_list::Op {
                scope: placement_scope(),
                table: PLACEMENT_TABLE.to_owned(),
                cursor: None,
                limit: None,
                order: None,
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send list request: {}", err))?
        .map_err(|err| custom_err!("unable to list placements: {}", err.message))?;

    response
        .entities
        .into_iter()
        .map(|(_id, value)| {
            postcard::from_bytes::<PlacementEntry>(&value)
                .map_err(|_| custom_err!("failed to deserialize placement entry"))
        })
        .collect()
}

/// Returns the placements of all currently placed cells.
pub async fn list_placements(session: &Session) -> Result<Vec<PlacementEntry>> {
    let db = DbClient::new(session);

    let response = db
        .read_tx_in(placement_scope(), async move |client, tx_id| {
            let request = tb_list::Request {
                id: tx_id,
                op: tb_list::Op {
                    scope: placement_scope(),
                    table: PLACEMENT_TABLE.to_owned(),
                    cursor: None,
                    limit: None,
                    order: None,
                },
            };
            client.send(request).await
        })
        .await
        .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
        .map_err(|err| custom_err!("unable to list table: {}", err.message))?;

    response
        .entities
        .into_iter()
        .map(|(_id, value)| {
            postcard::from_bytes::<PlacementEntry>(&value)
                .map_err(|_| custom_err!("failed to deserialize placement entry"))
        })
        .collect()
}

/// Returns the placement of a cell, or `None` if it has none.
pub async fn get_placement(session: &Session, sri: &Sri) -> Result<Option<PlacementEntry>> {
    let db = DbClient::new(session);
    let eid = sri.to_string().into_bytes();

    let response = db
        .read_tx_in(placement_scope(), async move |client, tx_id| {
            let request = tb_get::Request {
                id: tx_id,
                op: tb_get::Op {
                    scope: placement_scope(),
                    table: PLACEMENT_TABLE.to_owned(),
                    eid,
                },
            };
            client.send(request).await
        })
        .await
        .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
        .map_err(|err| custom_err!("unable to get entity: {}", err.message))?;

    match response.value {
        Some(bytes) => {
            let entry = postcard::from_bytes::<PlacementEntry>(&bytes)
                .map_err(|_| custom_err!("failed to deserialize placement entry"))?;
            Ok(Some(entry))
        }
        None => Ok(None),
    }
}

/// Returns a cell's placement within an existing transaction.
pub async fn get_placement_in_tx(
    db: &DbClient,
    tx_id: TxId,
    sri: &Sri,
) -> Result<Option<PlacementEntry>> {
    let response = db
        .send(tb_get::Request {
            id: tx_id,
            op: tb_get::Op {
                scope: placement_scope(),
                table: PLACEMENT_TABLE.to_owned(),
                eid: sri.to_string().into_bytes(),
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send get request: {}", err))?
        .map_err(|err| custom_err!("unable to get placement: {}", err.message))?;

    response
        .value
        .map(|bytes| {
            postcard::from_bytes::<PlacementEntry>(&bytes)
                .map_err(|_| custom_err!("failed to deserialize placement entry"))
        })
        .transpose()
}

/// Checks whether a cell has a placement within an existing transaction.
pub async fn placement_exists_in_tx(db: &DbClient, tx_id: TxId, sri: &Sri) -> Result<bool> {
    let response = db
        .send(tb_get::Request {
            id: tx_id,
            op: tb_get::Op {
                scope: placement_scope(),
                table: PLACEMENT_TABLE.to_owned(),
                eid: sri.to_string().into_bytes(),
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send get request: {}", err))?
        .map_err(|err| custom_err!("unable to get placement: {}", err.message))?;

    Ok(response.value.is_some())
}

/// Checks whether the cell with the given SRI has a placement.
pub async fn placement_exists(session: &Session, sri: &Sri) -> Result<bool> {
    let db = DbClient::new(session);
    let eid = sri.to_string().into_bytes();

    let found = db
        .read_tx_in(placement_scope(), async move |client, tx_id| {
            let request = tb_get::Request {
                id: tx_id,
                op: tb_get::Op {
                    scope: placement_scope(),
                    table: PLACEMENT_TABLE.to_owned(),
                    eid,
                },
            };
            client.send(request).await
        })
        .await
        .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
        .map_err(|err| custom_err!("unable to get placement: {}", err.message))?
        .value
        .is_some();

    Ok(found)
}

pub async fn ensure_placement_exists(session: &Session, sri: &Sri) -> Result<(), &'static str> {
    check_presence(placement_exists(session, sri).await, sri)
}

/// [`ensure_placement_exists`] inside an existing transaction.
pub async fn ensure_placement_exists_in_tx(
    db: &DbClient,
    tx_id: TxId,
    sri: &Sri,
) -> Result<(), &'static str> {
    check_presence(placement_exists_in_tx(db, tx_id, sri).await, sri)
}

fn check_presence(present: Result<bool>, sri: &Sri) -> Result<(), &'static str> {
    match present {
        Ok(true) => Ok(()),
        Ok(false) => {
            tracing::warn!("cell not found: {}", sri);
            Err("cell not found")
        }
        Err(err) => {
            tracing::warn!("unable to lookup [{}]: {}", sri, err);
            Err("placement check failed")
        }
    }
}

/// Outcome of claiming a cell SRI via [`claim_placement`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementClaimOutcome {
    /// The SRI was newly claimed with a Placeholder entry.
    Claimed,
    /// A placement with this SRI already exists.
    AlreadyExists,
}

/// Atomically claims a cell SRI with a Placeholder entry. Call
/// [`commit_placement`] after a successful deploy to replace the Placeholder
/// with the real `PlacementKind`; on deploy failure, [`remove_placement`] it
/// instead. Both are fenced by the generation the placeholder carries.
pub async fn claim_placement(
    session: &Session,
    entry: PlacementEntry,
) -> Result<PlacementClaimOutcome> {
    let db = DbClient::new(session);
    let eid = entry.sri.to_string().as_bytes().to_vec();
    let value = postcard::to_allocvec(&entry)
        .map_err(|_| custom_err!("failed to serialize placement entry"))?;

    db.write_tx_in(placement_scope(), async move |client, tx_id| {
        Ok(do_claim(client, tx_id, eid, value).await)
    })
    .await
    .map_err(|err| custom_err!("failed to write placement: {err}"))?
}

async fn do_claim(
    client: &DbClient,
    tx_id: TxId,
    eid: Vec<u8>,
    value: Vec<u8>,
) -> Result<PlacementClaimOutcome> {
    let existing = client
        .send(tb_get::Request {
            id: tx_id,
            op: tb_get::Op {
                scope: placement_scope(),
                table: PLACEMENT_TABLE.to_owned(),
                eid: eid.clone(),
            },
        })
        .await
        .map_err(|err| custom_err!("placement get failed: {err}"))?
        .map_err(|err| custom_err!("placement get failed: {}", err.message))?;

    if existing.value.is_some() {
        return Ok(PlacementClaimOutcome::AlreadyExists);
    }

    client
        .send(tb_insert::Request {
            id: tx_id,
            op: tb_insert::Op {
                scope: placement_scope(),
                table: PLACEMENT_TABLE.to_owned(),
                eid: Some(eid),
                value,
            },
        })
        .await
        .map_err(|err| custom_err!("placement insert failed: {err}"))?
        .map_err(|err| custom_err!("placement insert failed: {}", err.message))?;

    Ok(PlacementClaimOutcome::Claimed)
}

/// Replaces the Placeholder written by [`claim_placement`] with the real
/// `PlacementKind`, provided the row still carries `entry.gen_id`. A row of
/// another generation — or none, after an undeploy — means this deployment was
/// overtaken and must not publish itself; nothing is written then.
pub async fn commit_placement(session: &Session, entry: PlacementEntry) -> Result<FenceOutcome> {
    let db = DbClient::new(session);
    let value = postcard::to_allocvec(&entry)
        .map_err(|_| custom_err!("failed to serialize placement entry"))?;

    db.write_tx_in(placement_scope(), async move |client, tx_id| {
        Ok(do_commit(client, tx_id, &entry, value).await)
    })
    .await
    .map_err(|err| custom_err!("failed to write placement: {err}"))?
}

async fn do_commit(
    client: &DbClient,
    tx_id: TxId,
    entry: &PlacementEntry,
    value: Vec<u8>,
) -> Result<FenceOutcome> {
    let existing = get_placement_in_tx(client, tx_id, &entry.sri).await?;
    if let Err(outcome) = fence::admit(existing.map(|e| e.gen_id), entry.gen_id) {
        return Ok(outcome);
    }

    client
        .send(tb_insert::Request {
            id: tx_id,
            op: tb_insert::Op {
                scope: placement_scope(),
                table: PLACEMENT_TABLE.to_owned(),
                eid: Some(entry.sri.to_string().into_bytes()),
                value,
            },
        })
        .await
        .map_err(|err| custom_err!("placement insert failed: {err}"))?
        .map_err(|err| custom_err!("placement insert failed: {}", err.message))?;

    Ok(FenceOutcome::Applied)
}

/// Removes a cell's placement, provided the row still carries `gen_id`.
pub async fn remove_placement(session: &Session, sri: &Sri, gen_id: Gen) -> Result<FenceOutcome> {
    let db = DbClient::new(session);
    let sri = *sri;

    db.write_tx_in(placement_scope(), async move |client, tx_id| {
        Ok(do_remove(client, tx_id, &sri, gen_id).await)
    })
    .await
    .map_err(|err| custom_err!("failed to remove placement: {err}"))?
}

async fn do_remove(client: &DbClient, tx_id: TxId, sri: &Sri, gen_id: Gen) -> Result<FenceOutcome> {
    let existing = get_placement_in_tx(client, tx_id, sri).await?;
    if let Err(outcome) = fence::admit(existing.map(|e| e.gen_id), gen_id) {
        return Ok(outcome);
    }

    client
        .send(tb_delete::Request {
            id: tx_id,
            op: tb_delete::Op {
                scope: placement_scope(),
                table: PLACEMENT_TABLE.to_owned(),
                eid: sri.to_string().into_bytes(),
            },
        })
        .await
        .map_err(|err| custom_err!("placement delete failed: {err}"))?
        .map_err(|err| custom_err!("placement delete failed: {}", err.message))?;

    Ok(FenceOutcome::Applied)
}
