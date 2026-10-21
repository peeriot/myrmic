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
///
/// Also point-reads that generation's own key: listing the generations is not
/// tracked by the store's conflict check, a point read is, so a concurrent
/// write to the same generation on the same node fails one of the two commits.
pub(crate) async fn admit(
    client: &DbClient,
    tx_id: TxId,
    scope: &Scope,
    sri: &Sri,
    gen_id: Gen,
) -> Result<core::result::Result<(), FenceOutcome>> {
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
    let newest = generations(client, tx_id, scope, sri)
        .await?
        .into_iter()
        .max();

    Ok(fence::admit(newest, gen_id))
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

/// Adds a tombstone for `key`, so the row reads as gone.
pub(crate) async fn delete(
    client: &DbClient,
    tx_id: TxId,
    scope: &Scope,
    key: String,
) -> Result<()> {
    client
        .send(key_delete::Request {
            id: tx_id,
            op: key_delete::Op {
                scope: scope.clone(),
                key,
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send delete request: {err}"))?
        .map_err(|err| custom_err!("unable to delete row: {}", err.message))?;

    Ok(())
}
