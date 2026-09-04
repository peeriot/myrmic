use std::time::Duration;

use cell_protocol::{
    DEPLOYMENT_RESPONSES_TABLE, DeploymentConfirmation, RuntimeId, scope_of_deployment,
};
use sorg_common::{DbClient, SorgPayload, TxId, tb_delete, tb_list};

use crate::Result;
use crate::error::Error;
use crate::event_loop::Runtime;

const CONFIRMATION_POLL_INTERVAL: Duration = Duration::from_millis(100);

impl Runtime {
    /// Polls the embedded runtime's responses mailbox until a confirmation
    /// matching `predicate` arrives, consumes the entry, and returns the
    /// extracted value. Returns [`Error::DeploymentTimeout`] if no match
    /// arrives before `self.init_timeout`.
    pub(crate) async fn await_confirmation<T>(
        &self,
        target: RuntimeId,
        timeout_msg: String,
        predicate: impl Fn(DeploymentConfirmation) -> Option<T>,
    ) -> Result<T> {
        let deadline = tokio::time::Instant::now() + self.init_timeout;
        loop {
            if let Some(value) = self.poll_once(target, &predicate).await? {
                return Ok(value);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::DeploymentTimeout(timeout_msg));
            }
            tokio::time::sleep(CONFIRMATION_POLL_INTERVAL).await;
        }
    }

    async fn poll_once<T>(
        &self,
        target: RuntimeId,
        predicate: &impl Fn(DeploymentConfirmation) -> Option<T>,
    ) -> Result<Option<T>> {
        let db = DbClient::new(&self.session);

        // Claiming a confirmation is read-then-delete, so both halves share one
        // transaction and no two passes can take the same entry.
        db.write_tx_in(scope_of_deployment(target), async |client, tx_id| {
            Ok(claim(client, tx_id, target, predicate).await)
        })
        .await
        .map_err(|err| sorg_common::custom_err!("failed to claim a deployment response: {err}"))?
    }
}

async fn claim<T>(
    client: &DbClient,
    tx_id: TxId,
    target: RuntimeId,
    predicate: &impl Fn(DeploymentConfirmation) -> Option<T>,
) -> Result<Option<T>> {
    let scope = scope_of_deployment(target);
    let entities = tb_list(
        client.clone(),
        tx_id,
        scope.clone(),
        DEPLOYMENT_RESPONSES_TABLE.to_owned(),
        None,
        None,
        None,
    )
    .await
    .map_err(|err| {
        sorg_common::custom_err!("failed to read deployment responses from '{target}': {err}")
    })?;

    let Some((eid, value)) = entities.into_iter().find_map(|(eid, bytes)| {
        let confirmation =
            DeploymentConfirmation::from_slice(&bytes, "orch: deser confirmation").ok()?;
        predicate(confirmation).map(|v| (eid, v))
    }) else {
        return Ok(None);
    };

    tb_delete(
        client.clone(),
        tx_id,
        scope,
        DEPLOYMENT_RESPONSES_TABLE.to_owned(),
        eid,
    )
    .await
    .map_err(|err| {
        sorg_common::custom_err!("failed to delete confirmation entry from '{target}': {err}")
    })?;

    Ok(Some(value))
}
