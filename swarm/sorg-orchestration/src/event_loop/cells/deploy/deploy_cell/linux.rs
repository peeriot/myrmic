use cell_protocol::{ExecRuntimeInfo, Gen, PlacementKind, Sri};
use sorg_common::{
    SorgPayload, SpawnLineage, WasmCellDeployRequest, bail, topic_execution_cell_deploy, zenoh_err,
};
use tracing::debug;

use crate::Result;
use crate::event_loop::Runtime;

impl Runtime {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn deploy_wasm_cell_linux(
        &self,
        cell_sri: &Sri,
        class_name: &str,
        runtime: &ExecRuntimeInfo,
        gen_id: Gen,
        lineage: SpawnLineage,
        arguments: Option<Vec<u8>>,
    ) -> Result<PlacementKind> {
        let exec_request = WasmCellDeployRequest {
            cell_sri: *cell_sri,
            class_name: class_name.to_owned(),
            arguments,
            gen_id,
            lineage,
        };

        let target = runtime.id();
        let topic = topic_execution_cell_deploy(target);
        let fwd_payload = exec_request.to_payload()?;
        debug!("forwarding cell deploy to exec runtime '{target}' on topic '{topic}'");

        let reply = self
            .session
            .get(&topic)
            .payload(fwd_payload)
            .timeout(self.init_timeout)
            .await
            .map_err(|zen_err| zenoh_err!("orch cell deploy query to exec", zen_err))?;

        match reply.recv_async().await {
            Ok(reply) => match reply.result() {
                Ok(_sample) => Ok(PlacementKind::Wasm {
                    runtime: runtime.clone(),
                }),
                Err(err_reply) => {
                    let bytes = err_reply.payload().to_bytes();
                    let msg = String::from_utf8_lossy(&bytes);
                    bail!("exec runtime failed to deploy cell: {msg}");
                }
            },
            Err(err) => {
                bail!("no response from exec runtime for cell deploy: {err}");
            }
        }
    }
}
