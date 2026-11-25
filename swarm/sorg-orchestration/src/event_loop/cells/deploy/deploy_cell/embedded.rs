use cell_protocol::{
    ArtifactPlatform, BlobHash, DEPLOYMENT_TABLE, DeploymentCommand, DeploymentConfirmation,
    ExecRuntimeInfo, Gen, PlacementKind, Sri, scope_of_deployment,
};
use sorg_common::{DbClient, SorgPayload, SpawnLineage, bail, class_registry, tb_insert};

use crate::Result;
use crate::event_loop::Runtime;

impl Runtime {
    /// Deploys a wasm cell onto an embedded runtime via the DB-mailbox protocol:
    /// writes a [`DeploymentCommand`] into the runtime's deployment table, then
    /// awaits the [`DeploymentConfirmation`] it writes back.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn deploy_wasm_cell_embedded(
        &self,
        cell_sri: &Sri,
        class: BlobHash,
        runtime: &ExecRuntimeInfo,
        gen_id: Gen,
        lineage: SpawnLineage,
        payload: Option<Vec<u8>>,
    ) -> Result<PlacementKind> {
        let target = runtime.id();
        let sri = *cell_sri;

        // The board fetches the AOT binary by id and checks it against the CRC,
        // without reading the registry itself. Placement admitted this runtime
        // only for a class with an AOT for its platform.
        let Some((aot, crc)) =
            class_registry::aot_for(&self.session, class, ArtifactPlatform::Riscv32imac).await?
        else {
            bail!(
                "class {} has no AOT binary for {} (removed since placement?)",
                class.to_hex(),
                ArtifactPlatform::Riscv32imac
            );
        };

        // The firmware writes the instance row itself (before confirming),
        // so the command carries the full edge.
        let command = DeploymentCommand::Deploy {
            class,
            aot,
            crc,
            sri,
            payload,
            gen_id,
            lineage,
        };

        let bytes = command.to_bytes()?;
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
                sorg_common::custom_err!("failed to open deployment command tx for '{sri}': {err}")
            })?
            .map_err(|err| {
                sorg_common::custom_err!("failed to send deployment command for '{sri}': {err}")
            })?;

        let failure = self
            .await_confirmation(
                target,
                format!(
                    "timed out waiting for deployment confirmation from runtime '{target}' for cell '{sri}'"
                ),
                |c| match c {
                    DeploymentConfirmation::Deployed { failure, sri: s } if s == sri => {
                        Some(failure)
                    }
                    _ => None,
                },
            )
            .await?;

        match failure {
            None => Ok(PlacementKind::Wasm {
                runtime: runtime.clone(),
            }),
            Some(msg) => bail!(
                "embedded runtime '{target}' reported a deployment failure for '{sri}': {msg}"
            ),
        }
    }
}
