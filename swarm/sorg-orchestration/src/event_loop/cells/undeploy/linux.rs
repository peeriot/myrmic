use cell_protocol::{RuntimeId, Sri};
use sorg_common::{
    CellUndeployRequest, SorgPayload, bail, topic_execution_cell_undeploy, zenoh_err,
};
use tracing::debug;

use crate::Result;
use crate::event_loop::Runtime;

impl Runtime {
    pub(super) async fn undeploy_wasm_cell_linux(
        &self,
        cell_sri: &Sri,
        node_id: RuntimeId,
    ) -> Result<()> {
        let request = CellUndeployRequest {
            cell_sri: *cell_sri,
        };
        let topic = topic_execution_cell_undeploy(node_id);
        let fwd_payload = request.to_payload()?;
        debug!("forwarding cell undeploy to exec runtime '{node_id}' on topic '{topic}'");

        let reply = self
            .session
            .get(&topic)
            .payload(fwd_payload)
            .timeout(self.init_timeout)
            .await
            .map_err(|zen_err| zenoh_err!("orch cell undeploy query to exec", zen_err))?;

        match reply.recv_async().await {
            Ok(reply) => match reply.result() {
                Ok(_sample) => Ok(()),
                Err(err_reply) => {
                    let bytes = err_reply.payload().to_bytes();
                    let msg = String::from_utf8_lossy(&bytes);
                    bail!("exec runtime failed to undeploy cell: {msg}");
                }
            },
            Err(err) => {
                bail!("no response from exec runtime for cell undeploy: {err}");
            }
        }
    }
}
