use cell_protocol::{
    DEPLOYMENT_TABLE, DeploymentCommand, DeploymentConfirmation, ExecRuntimeInfo, Sri,
    scope_of_deployment,
};
use sorg_common::{DbClient, SorgPayload, tb_insert};

use crate::Result;
use crate::event_loop::Runtime;

impl Runtime {
    pub(super) async fn undeploy_wasm_cell_embedded(
        &self,
        cell_sri: &Sri,
        runtime: &ExecRuntimeInfo,
    ) -> Result<()> {
        let target = runtime.id();
        let sri = *cell_sri;

        let bytes = DeploymentCommand::Delete { sri }.to_bytes()?;
        DbClient::new(&self.session)
            .write_tx_in(scope_of_deployment(target), async |client, tx_id| {
                Ok(tb_insert(
                    client.clone(),
                    tx_id,
                    scope_of_deployment(target),
                    DEPLOYMENT_TABLE.to_owned(),
                    None,
                    bytes,
                )
                .await)
            })
            .await
            .map_err(|err| {
                sorg_common::custom_err!(
                    "failed to open delete command tx for cell '{cell_sri}': {err}"
                )
            })?
            .map_err(|err| {
                sorg_common::custom_err!(
                    "failed to send delete command for cell '{cell_sri}' to runtime '{target}': {err}"
                )
            })?;

        self.await_confirmation(
            target,
            format!(
                "timed out waiting for delete confirmation from runtime '{target}' for cell '{cell_sri}'"
            ),
            |c| match c {
                DeploymentConfirmation::Deleted { sri: ref s } if *s == sri => Some(()),
                _ => None,
            },
        )
        .await
    }
}
