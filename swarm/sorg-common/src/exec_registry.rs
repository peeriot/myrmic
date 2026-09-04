use cell_protocol::{EXEC_REGISTRY_TABLE, RuntimeId, scope_of_exec_registry};
use db_client::v1::{
    Client as DbClient,
    models::{TxId, tb_delete, tb_insert, tb_list},
};
use zenoh::{Session, config::ZenohId};

use tracing::debug;

use crate::{ExecRuntimeInfo, Result, custom_err};

pub async fn list_registered_execs(session: &Session) -> Result<Vec<ExecRuntimeInfo>> {
    let db = DbClient::new(session);

    db.read_tx_in(scope_of_exec_registry(), async move |client, tx_id| {
        Ok(do_list(client, tx_id).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

/// Reads the exec registry within a committed read transaction. The closure
/// runs before the commit, so the commit provides an optimistic concurrency
/// check: if the registry changed between the read and the commit, the
/// function returns an error and the caller can retry with fresh data.
pub async fn with_registered_execs<F, R>(session: &Session, f: F) -> Result<R>
where
    F: FnOnce(Vec<ExecRuntimeInfo>) -> R,
{
    let db = DbClient::new(session);
    db.read_tx_in(scope_of_exec_registry(), async move |client, tx_id| {
        let entries = do_list(client, tx_id).await;
        Ok(entries.map(f))
    })
    .await
    .map_err(|err| custom_err!("exec registry read failed: {}", err))?
}

pub async fn register_exec(session: &Session, info: &ExecRuntimeInfo) -> Result<()> {
    let info = info.clone();
    let db = DbClient::new(session);

    db.write_tx_in(scope_of_exec_registry(), async move |client, tx_id| {
        Ok(do_register(client, tx_id, &info).await)
    })
    .await
    .map_err(|err| custom_err!("unable to communicate with db: {}", err))?
}

/// Re-registers this exec if its registry row has gone missing or no longer
/// describes it. A stale leave-deregistration racing a restart (peers retry
/// deregistering a dropped liveliness token for ~10s, while the node
/// re-registers within ~2s), or a hygiene pass during a lease outage, can
/// delete a live exec's row — and boot-time registration never re-runs. A row
/// that survived but went stale means a retag failed to publish, which this
/// repairs on the same slow cadence. Returns whether a repair was needed.
pub async fn ensure_registered(session: &Session, info: &ExecRuntimeInfo) -> Result<bool> {
    let current = with_registered_execs(session, |execs| {
        execs.iter().find(|e| e.id() == info.id()).cloned()
    })
    .await?;

    if current.as_ref() == Some(info) {
        return Ok(false);
    }

    register_exec(session, info).await?;
    Ok(true)
}

pub async fn deregister_exec(session: &Session, leaving_id: ZenohId) -> Result<()> {
    deregister_exec_by_runtime_id(session, leaving_id.into()).await
}

pub async fn deregister_exec_by_runtime_id(session: &Session, runtime_id: RuntimeId) -> Result<()> {
    let id_bytes = runtime_id.to_string().into_bytes();
    let db = DbClient::new(session);

    let tx_result = db
        .write_tx_in(scope_of_exec_registry(), async move |client, tx_id| {
            Ok(do_check_and_deregister(client, tx_id, runtime_id, &id_bytes).await)
        })
        .await;

    match tx_result {
        Ok(inner) => {
            debug!("deregister write tx success for {runtime_id}");
            inner
        }
        Err(err) => {
            debug!("deregister write-tx failed for {runtime_id}: {err}");
            // Commit may have failed because another node already removed the entry.
            let execs = list_registered_execs(session).await?;
            if execs.iter().any(|e| e.id() == runtime_id) {
                Err(custom_err!(
                    "failed to deregister exec {runtime_id} from registry"
                ))
            } else {
                debug!("deregistered by a different node");
                Ok(())
            }
        }
    }
}

pub async fn list_execs(client: &DbClient, tx_id: TxId) -> Result<Vec<ExecRuntimeInfo>> {
    do_list(client, tx_id).await
}

async fn do_list(client: &DbClient, tx_id: TxId) -> Result<Vec<ExecRuntimeInfo>> {
    let response = client
        .send(tb_list::Request {
            id: tx_id,
            op: tb_list::Op {
                scope: scope_of_exec_registry(),
                table: EXEC_REGISTRY_TABLE.to_owned(),
                cursor: None,
                limit: None,
                order: None,
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send list request: {}", err))?
        .map_err(|err| custom_err!("unable to list execs: {}", err.message))?;

    response
        .entities
        .into_iter()
        .map(|(_id, value)| {
            postcard::from_bytes(&value)
                .map_err(|_| custom_err!("failed to deserialize exec registry entry"))
        })
        .collect()
}

async fn do_register(client: &DbClient, tx_id: TxId, info: &ExecRuntimeInfo) -> Result<()> {
    let value = postcard::to_allocvec(info)
        .map_err(|_| custom_err!("failed to serialize exec registry entry"))?;

    client
        .send(tb_insert::Request {
            id: tx_id,
            op: tb_insert::Op {
                scope: scope_of_exec_registry(),
                table: EXEC_REGISTRY_TABLE.to_owned(),
                eid: Some(info.id().to_string().into_bytes()),
                value,
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send insert request: {}", err))?
        .map_err(|err| custom_err!("unable to register exec: {}", err.message))?;

    Ok(())
}

async fn do_check_and_deregister(
    client: &DbClient,
    tx_id: TxId,
    runtime_id: RuntimeId,
    id_bytes: &[u8],
) -> Result<()> {
    let entries = do_list(client, tx_id).await?;
    if entries.iter().any(|e| e.id() == runtime_id) {
        do_deregister(client, tx_id, id_bytes).await?;
    }
    Ok(())
}

async fn do_deregister(client: &DbClient, tx_id: TxId, id_bytes: &[u8]) -> Result<()> {
    client
        .send(tb_delete::Request {
            id: tx_id,
            op: tb_delete::Op {
                scope: scope_of_exec_registry(),
                table: EXEC_REGISTRY_TABLE.to_owned(),
                eid: id_bytes.to_vec(),
            },
        })
        .await
        .map_err(|err| custom_err!("unable to send delete request: {}", err))?
        .map_err(|err| custom_err!("unable to deregister exec: {}", err.message))?;

    Ok(())
}
