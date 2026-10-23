//! Reading and writing the per-generation rows of placements and instances
//! (see [`cell_protocol::generations`]) inside a transaction.

use cell_protocol::generations::{generation_key, generations_of, parse_generation_key};
use cell_protocol::{Gen, Sri};
use db_client::v1::{
    Client as DbClient,
    models::{Scope, TxId, key_delete, key_get, key_list, key_prefix, key_put},
};

use super::fence::{self, FenceOutcome};
use crate::{Result, custom_err};

/// [`fence::admit`] for a write to generation `gen_id` of `sri` in `scope`:
/// the cell's newest generation must be `gen_id`.
pub(crate) async fn admit(
    client: &DbClient,
    tx_id: TxId,
    scope: &Scope,
    sri: &Sri,
    gen_id: Gen,
) -> Result<core::result::Result<(), FenceOutcome>> {
    let newest = newest_generation(client, tx_id, scope, sri, gen_id).await?;

    Ok(fence::admit(newest, gen_id))
}

/// Removes generation `gen_id` of `sri`'s row in `scope`. A key per generation
/// means this can't touch another generation, so it is never refused. The
/// outcome reports the cell's newest generation as this transaction sees it,
/// the way [`admit`] does for a write.
///
/// The tombstone is written whether or not this transaction sees the row: its
/// holder can be a write behind on `scope` (a caller's transaction routed to
/// another scope), and a tombstone for a row that is already gone is harmless.
pub(crate) async fn remove(
    client: &DbClient,
    tx_id: TxId,
    scope: &Scope,
    sri: &Sri,
    gen_id: Gen,
) -> Result<FenceOutcome> {
    let newest = newest_generation(client, tx_id, scope, sri, gen_id).await?;

    client
        .send(key_delete::Request {
            id: tx_id,
            op: key_delete::Op {
                scope: scope.clone(),
                key: generation_key(sri, gen_id),
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send delete request: {err}"))?
        .map_err(|err| custom_err!("unable to remove row: {}", err.message))?;

    Ok(match fence::admit(newest, gen_id) {
        Ok(()) => FenceOutcome::Applied,
        Err(outcome) => outcome,
    })
}

/// The newest generation of `sri` in `scope`, for a mutation of generation
/// `gen_id`.
///
/// Also point-reads `gen_id`'s own key: listing the generations is not tracked
/// by the store's conflict check, a point read is, so a concurrent write to the
/// same generation on the same node fails one of the two commits.
async fn newest_generation(
    client: &DbClient,
    tx_id: TxId,
    scope: &Scope,
    sri: &Sri,
    gen_id: Gen,
) -> Result<Option<Gen>> {
    client
        .send(key_get::Request {
            id: tx_id,
            op: key_get::Op {
                scope: scope.clone(),
                key: generation_key(sri, gen_id),
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send get request: {err}"))?
        .map_err(|err| custom_err!("unable to read generation: {}", err.message))?;

    Ok(generations(client, tx_id, scope, sri)
        .await?
        .into_iter()
        .max())
}

/// Every generation of `sri` in `scope`.
pub(crate) async fn generations(
    client: &DbClient,
    tx_id: TxId,
    scope: &Scope,
    sri: &Sri,
) -> Result<Vec<Gen>> {
    let keys = client
        .send(key_prefix::Request {
            id: tx_id,
            op: key_prefix::Op {
                scope: scope.clone(),
                prefix: generations_of(sri),
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send prefix request: {err}"))?
        .map_err(|err| custom_err!("unable to list generations: {}", err.message))?
        .keys;

    keys.iter()
        .map(|key| {
            parse_generation_key(key)
                .map(|(_, gen_id)| gen_id)
                .ok_or_else(|| custom_err!("malformed generation key {key:?}"))
        })
        .collect()
}

/// The rows (keys with values) under `prefix` in `scope`.
pub(crate) async fn list(
    client: &DbClient,
    tx_id: TxId,
    scope: &Scope,
    prefix: String,
) -> Result<Vec<(String, Vec<u8>)>> {
    Ok(client
        .send(key_list::Request {
            id: tx_id,
            op: key_list::Op {
                scope: scope.clone(),
                prefix,
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send list request: {err}"))?
        .map_err(|err| custom_err!("unable to list rows: {}", err.message))?
        .entries)
}

pub(crate) async fn put(
    client: &DbClient,
    tx_id: TxId,
    scope: &Scope,
    key: String,
    value: Vec<u8>,
) -> Result<()> {
    client
        .send(key_put::Request {
            id: tx_id,
            op: key_put::Op {
                scope: scope.clone(),
                key,
                value,
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send put request: {err}"))?
        .map_err(|err| custom_err!("unable to write row: {}", err.message))?;

    Ok(())
}
