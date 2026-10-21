use cell_protocol::generations::{GENERATIONS, generation_key, generations_of, newest_placements};
use cell_protocol::{Gen, PlacementEntry, Sri, placement_claim_key, placement_scope};
use db_client::v1::{
    Client as DbClient,
    models::{TxId, key_get, key_prefix},
};
use zenoh::Session;

use super::fence::FenceOutcome;
use super::generation_rows;
use crate::{Result, custom_err};

/// Returns the placements of all currently placed cells: each cell's newest
/// generation.
pub async fn list_placements(session: &Session) -> Result<Vec<PlacementEntry>> {
    let db = DbClient::new(session);

    let entries = db
        .read_tx_in(placement_scope(), async move |client, tx_id| {
            Ok(
                generation_rows::list(client, tx_id, &placement_scope(), GENERATIONS.to_owned())
                    .await,
            )
        })
        .await
        .map_err(|err| custom_err!("unable to communicate with db: {}", err))??;

    newest_placements(entries)
        .into_iter()
        .map(|placement| placement.map_err(|err| custom_err!("unable to read placements: {err}")))
        .collect()
}

/// Returns the placement of a cell, its newest generation, or `None` if it has
/// none.
pub async fn get_placement(session: &Session, sri: &Sri) -> Result<Option<PlacementEntry>> {
    let db = DbClient::new(session);
    let sri = *sri;

    db.read_tx_in(placement_scope(), async move |client, tx_id| {
        Ok(get_placement_in_tx(client, tx_id, &sri).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

/// Returns a cell's placement within an existing transaction. Private on
/// purpose: an op carries its own scope wherever it lands, so a placement read
/// issued on someone else's transaction is answered by that scope's holder,
/// which can be a write behind on this one. Callers outside this module take
/// [`get_placement`], which routes by the placement scope's own locate round.
async fn get_placement_in_tx(
    db: &DbClient,
    tx_id: TxId,
    sri: &Sri,
) -> Result<Option<PlacementEntry>> {
    let entries = generation_rows::list(db, tx_id, &placement_scope(), generations_of(sri)).await?;

    let mut placements = newest_placements(entries)
        .into_iter()
        .collect::<core::result::Result<Vec<_>, _>>()
        .map_err(|err| custom_err!("unable to read placement: {err}"))?;

    // The prefix covers this one SRI, so at most one placement comes back.
    Ok(placements.pop())
}

/// Checks whether the cell with the given SRI has a placement.
pub async fn placement_exists(session: &Session, sri: &Sri) -> Result<bool> {
    let db = DbClient::new(session);
    let prefix = generations_of(sri);

    let keys = db
        .read_tx_in(placement_scope(), async move |client, tx_id| {
            let request = key_prefix::Request {
                id: tx_id,
                op: key_prefix::Op {
                    scope: placement_scope(),
                    prefix,
                },
            };
            client.send(request).await
        })
        .await
        .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
        .map_err(|err| custom_err!("unable to get placement: {}", err.message))?
        .keys;

    Ok(!keys.is_empty())
}

pub async fn ensure_placement_exists(session: &Session, sri: &Sri) -> Result<(), &'static str> {
    check_presence(placement_exists(session, sri).await, sri)
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

    db.write_tx_in(placement_scope(), async move |client, tx_id| {
        Ok(do_claim(client, tx_id, &entry).await)
    })
    .await
    .map_err(|err| custom_err!("failed to write placement: {err}"))?
}

async fn do_claim(
    client: &DbClient,
    tx_id: TxId,
    entry: &PlacementEntry,
) -> Result<PlacementClaimOutcome> {
    let scope = placement_scope();

    // Read and written by every claim of this SRI, so two concurrent claims
    // conflict at commit; the generation listing below is not conflict-tracked.
    let claim_key = placement_claim_key(&entry.sri);
    client
        .send(key_get::Request {
            id: tx_id,
            op: key_get::Op {
                scope: scope.clone(),
                key: claim_key.clone(),
            },
        })
        .await
        .map_err(|err| custom_err!("placement claim read failed: {err}"))?
        .map_err(|err| custom_err!("placement claim read failed: {}", err.message))?;

    if get_placement_in_tx(client, tx_id, &entry.sri)
        .await?
        .is_some()
    {
        return Ok(PlacementClaimOutcome::AlreadyExists);
    }

    generation_rows::put(client, tx_id, &scope, claim_key, vec![]).await?;
    put_placement(client, tx_id, entry).await?;

    Ok(PlacementClaimOutcome::Claimed)
}

/// Replaces the Placeholder written by [`claim_placement`] with the real
/// `PlacementKind`, provided the cell's newest generation is still
/// `entry.gen_id`. Another generation, or none after an undeploy, means this
/// deployment was overtaken and must not publish itself; nothing is written
/// then.
pub async fn commit_placement(session: &Session, entry: PlacementEntry) -> Result<FenceOutcome> {
    let db = DbClient::new(session);

    db.write_tx_in(placement_scope(), async move |client, tx_id| {
        Ok(do_commit(client, tx_id, &entry).await)
    })
    .await
    .map_err(|err| custom_err!("failed to write placement: {err}"))?
}

async fn do_commit(client: &DbClient, tx_id: TxId, entry: &PlacementEntry) -> Result<FenceOutcome> {
    let admitted =
        generation_rows::admit(client, tx_id, &placement_scope(), &entry.sri, entry.gen_id).await?;
    if let Err(outcome) = admitted {
        return Ok(outcome);
    }

    put_placement(client, tx_id, entry).await?;

    Ok(FenceOutcome::Applied)
}

/// Removes generation `gen_id` of a cell's placement, provided it is the
/// cell's newest generation.
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
    let scope = placement_scope();
    if let Err(outcome) = generation_rows::admit(client, tx_id, &scope, sri, gen_id).await? {
        return Ok(outcome);
    }

    generation_rows::delete(client, tx_id, &scope, generation_key(sri, gen_id)).await?;

    Ok(FenceOutcome::Applied)
}

async fn put_placement(client: &DbClient, tx_id: TxId, entry: &PlacementEntry) -> Result<()> {
    let value = postcard::to_allocvec(&entry.row())
        .map_err(|_| custom_err!("failed to serialize placement row"))?;

    generation_rows::put(
        client,
        tx_id,
        &placement_scope(),
        generation_key(&entry.sri, entry.gen_id),
        value,
    )
    .await
}
