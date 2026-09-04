//! Pending root-death signals: the transient exec → orchestrator channel that
//! carries *why* a root died when it dies on a still-live node (crash, clean
//! stop, terminate). Node-loss deaths need no signal — the orchestrator's
//! hygiene pass detects those directly. An exec writes one row per root death;
//! the orchestrator consumes it (restart or give up) and deletes it, so rows
//! are short-lived and a reconciliation drops any that lack a matching spec.

use cell_protocol::{Gen, ROOT_DEATH_TABLE, Sri, root_death_scope};
use db_client::v1::{
    Client as DbClient,
    models::{TxId, tb_delete, tb_get, tb_insert, tb_list},
};
use myrmic_common::cells::LostReason;
use serde::{Deserialize, Serialize};
use zenoh::Session;

use crate::{Result, custom_err};

/// A root that died on a live node, and why. `gen_id` is the dead instance's
/// generation, so the orchestrator can tell an uncleaned corpse from a root
/// already restarted at a newer generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootDeath {
    pub sri: Sri,
    pub gen_id: Gen,
    pub reason: LostReason,
}

/// Records a root's death (last write wins for a given SRI).
pub async fn record(session: &Session, sri: Sri, gen_id: Gen, reason: LostReason) -> Result<()> {
    let death = RootDeath {
        sri,
        gen_id,
        reason,
    };
    let db = DbClient::new(session);
    db.write_tx_in(root_death_scope(), async move |client, tx_id| {
        Ok(do_record(client, tx_id, &death).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

/// Lists all pending root-death signals.
pub async fn list(session: &Session) -> Result<Vec<RootDeath>> {
    let db = DbClient::new(session);
    db.read_tx_in(root_death_scope(), async move |client, tx_id| {
        Ok(do_list(client, tx_id).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

/// Clears a pending root-death signal, returning whether one was removed.
pub async fn clear(session: &Session, sri: &Sri) -> Result<bool> {
    let sri = *sri;
    let db = DbClient::new(session);
    db.write_tx_in(root_death_scope(), async move |client, tx_id| {
        Ok(do_clear(client, tx_id, &sri).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

async fn do_record(client: &DbClient, tx_id: TxId, death: &RootDeath) -> Result<()> {
    let value =
        postcard::to_allocvec(death).map_err(|_| custom_err!("failed to serialize root death"))?;
    client
        .send(tb_insert::Request {
            id: tx_id,
            op: tb_insert::Op {
                scope: root_death_scope(),
                table: ROOT_DEATH_TABLE.to_owned(),
                eid: Some(death.sri.to_string().into_bytes()),
                value,
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send insert request: {}", err))?
        .map_err(|err| custom_err!("unable to insert root death: {}", err.message))?;
    Ok(())
}

async fn do_list(client: &DbClient, tx_id: TxId) -> Result<Vec<RootDeath>> {
    let response = client
        .send(tb_list::Request {
            id: tx_id,
            op: tb_list::Op {
                scope: root_death_scope(),
                table: ROOT_DEATH_TABLE.to_owned(),
                cursor: None,
                limit: None,
                order: None,
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send list request: {}", err))?
        .map_err(|err| custom_err!("unable to list root deaths: {}", err.message))?;

    let mut deaths = Vec::with_capacity(response.entities.len());
    for (_id, value_bytes) in response.entities {
        let record = postcard::from_bytes::<RootDeath>(&value_bytes)
            .map_err(|_| custom_err!("failed to deserialize root death"))?;
        deaths.push(record);
    }
    Ok(deaths)
}

async fn do_clear(client: &DbClient, tx_id: TxId, sri: &Sri) -> Result<bool> {
    let exists = client
        .send(tb_get::Request {
            id: tx_id,
            op: tb_get::Op {
                scope: root_death_scope(),
                table: ROOT_DEATH_TABLE.to_owned(),
                eid: sri.to_string().into_bytes(),
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send get request: {}", err))?
        .map_err(|err| custom_err!("unable to get root death: {}", err.message))?
        .value
        .is_some();
    if !exists {
        return Ok(false);
    }
    client
        .send(tb_delete::Request {
            id: tx_id,
            op: tb_delete::Op {
                scope: root_death_scope(),
                table: ROOT_DEATH_TABLE.to_owned(),
                eid: sri.to_string().into_bytes(),
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send delete request: {}", err))?
        .map_err(|err| custom_err!("unable to delete root death: {}", err.message))?;
    Ok(true)
}
