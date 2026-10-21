use cell_protocol::generations::{GENERATIONS, generation_key, generations_of, newest_instances};
use cell_protocol::{CellInstance, Gen, Sri, instance_registry_scope};
use db_client::v1::{Client as DbClient, models::TxId};
use zenoh::Session;

use super::fence::{self, FenceOutcome};
use super::{generation_rows, placement};
use crate::{Result, bail, custom_err};

/// Erases the instance row of one incarnation, provided it is still the cell's
/// newest generation `gen_id`. Refused while that incarnation is deployed: a
/// placement of the same generation means the row is a live cell's record. An
/// absent row is reported, not an error — callers routinely race undeploy's
/// own erase.
pub async fn erase_instance(session: &Session, sri: &Sri, gen_id: Gen) -> Result<FenceOutcome> {
    // Each row is read through its own scope's locate round. Issued on this
    // function's transaction, a placement read is answered by the node holding
    // the instance registry, which can be a write behind on the placement scope
    // and then reports a placement that is already gone.
    //
    // The row's own generation decides first: a row of another incarnation is
    // not this caller's to erase, whatever the placement says.
    let existing = get_instance(session, sri).await?;
    if let Err(outcome) = fence::admit(existing.map(|record| record.gen_id), gen_id) {
        return Ok(outcome);
    }
    if placement::get_placement(session, sri)
        .await?
        .is_some_and(|entry| entry.gen_id == gen_id)
    {
        bail!(
            "cannot erase instance '{}': cell is currently deployed",
            sri
        );
    }

    let sri = *sri;
    let db = DbClient::new(session);

    db.write_tx_in(instance_registry_scope(), async move |client, tx_id| {
        Ok(do_erase(client, tx_id, &sri, gen_id).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

/// Returns every cell's instance: its newest generation.
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
/// keyed by `record.sri` and `record.gen_id`.
pub async fn insert_registry_entry_in_tx(
    session: &Session,
    tx_id: TxId,
    record: &CellInstance,
) -> Result<()> {
    // Read through the registry's own locate round: the caller's transaction is
    // routed to its own scope, whose holder can be a write behind on this one.
    let sri = record.sri;
    let generations = DbClient::new(session)
        .read_tx_in(instance_registry_scope(), async move |client, tx_id| {
            Ok(generation_rows::generations(client, tx_id, &instance_registry_scope(), &sri).await)
        })
        .await
        .map_err(|err| custom_err!("unable to communicate with db: {}", err))??;

    let db = DbClient::new(session);
    do_insert_registry_entry(&db, tx_id, record, generations).await
}

/// Inserts a registry entry in its own write transaction.
pub async fn insert_registry_entry(session: &Session, record: &CellInstance) -> Result<()> {
    let record = record.clone();
    let db = DbClient::new(session);

    db.write_tx_in(instance_registry_scope(), async move |client, tx_id| {
        Ok(async {
            let generations = generation_rows::generations(
                client,
                tx_id,
                &instance_registry_scope(),
                &record.sri,
            )
            .await?;
            do_insert_registry_entry(client, tx_id, &record, generations).await
        }
        .await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

/// Returns a cell's instance, its newest generation, or `None` if it has none.
pub async fn get_instance(session: &Session, sri: &Sri) -> Result<Option<CellInstance>> {
    let sri = *sri;
    let db = DbClient::new(session);

    db.read_tx_in(instance_registry_scope(), async move |client, tx_id| {
        Ok(do_get(client, tx_id, &sri).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

async fn do_erase(client: &DbClient, tx_id: TxId, sri: &Sri, gen_id: Gen) -> Result<FenceOutcome> {
    let scope = instance_registry_scope();
    if let Err(outcome) = generation_rows::admit(client, tx_id, &scope, sri, gen_id).await? {
        return Ok(outcome);
    }

    generation_rows::delete(client, tx_id, &scope, generation_key(sri, gen_id)).await?;

    Ok(FenceOutcome::Applied)
}

async fn do_list(client: &DbClient, tx_id: TxId) -> Result<Vec<CellInstance>> {
    let entries = generation_rows::list(
        client,
        tx_id,
        &instance_registry_scope(),
        GENERATIONS.to_owned(),
    )
    .await?;

    newest_instances(entries)
        .into_iter()
        .map(|instance| instance.map_err(|err| custom_err!("unable to read instances: {err}")))
        .collect()
}

async fn do_get(client: &DbClient, tx_id: TxId, sri: &Sri) -> Result<Option<CellInstance>> {
    let entries = generation_rows::list(
        client,
        tx_id,
        &instance_registry_scope(),
        generations_of(sri),
    )
    .await?;
    let mut instances = newest_instances(entries)
        .into_iter()
        .collect::<core::result::Result<Vec<_>, _>>()
        .map_err(|err| custom_err!("unable to read instance: {err}"))?;

    // The prefix covers this one SRI, so at most one instance comes back.
    Ok(instances.pop())
}

/// Writes `record`'s generation and tombstones the older ones among the SRI's
/// `generations` in the same transaction: an older generation left behind
/// would come back into view once this one is erased.
async fn do_insert_registry_entry(
    client: &DbClient,
    tx_id: TxId,
    record: &CellInstance,
    generations: Vec<Gen>,
) -> Result<()> {
    let scope = instance_registry_scope();
    let older = generations
        .into_iter()
        .filter(|gen_id| *gen_id < record.gen_id);
    for gen_id in older {
        generation_rows::delete(client, tx_id, &scope, generation_key(&record.sri, gen_id)).await?;
    }

    let value = postcard::to_allocvec(&record.row())
        .map_err(|_| custom_err!("failed to serialize instance row"))?;
    generation_rows::put(
        client,
        tx_id,
        &scope,
        generation_key(&record.sri, record.gen_id),
        value,
    )
    .await
}
