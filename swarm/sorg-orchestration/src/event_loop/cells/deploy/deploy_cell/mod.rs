mod bridges;
mod embedded;
mod linux;

use cell_protocol::Gen;
use cell_protocol::{ExecRuntimeInfo, PlacementKind, RuntimeKind, Sri};
use sorg_common::{CellConfig, SpawnLineage, bail};

use crate::Result;
use crate::event_loop::Runtime;

impl Runtime {
    pub(crate) async fn deploy_cell(
        &self,
        sri: &Sri,
        config: CellConfig,
        runtime: &ExecRuntimeInfo,
        gen_id: Gen,
        lineage: SpawnLineage,
        arguments: Option<Vec<u8>>,
    ) -> Result<PlacementKind> {
        match config {
            CellConfig::Wasm { class } => {
                self.deploy_wasm_cell(sri, &class, runtime, gen_id, lineage, arguments)
                    .await
            }
            // Bridge cells run natively on the orchestrator (see `bridges.rs`), not on
            // the placed exec runtime: they only need a mailbox listener, not a WASM
            // execution engine. Bridges are always roots, so no lineage to thread.
            CellConfig::HttpBridge(api) => self.deploy_http_bridge(sri, api).await,
            CellConfig::MqttBridge(bridge) => self.deploy_mqtt_bridge(sri, bridge).await,
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn deploy_wasm_cell(
        &self,
        cell_sri: &Sri,
        class_name: &str,
        runtime: &ExecRuntimeInfo,
        gen_id: Gen,
        lineage: SpawnLineage,
        arguments: Option<Vec<u8>>,
    ) -> Result<PlacementKind> {
        match runtime.runtime_kind() {
            RuntimeKind::Linux => {
                self.deploy_wasm_cell_linux(
                    cell_sri, class_name, runtime, gen_id, lineage, arguments,
                )
                .await
            }
            RuntimeKind::Esp32c5 | RuntimeKind::Esp32c6 | RuntimeKind::Esp32c61 => {
                // esp32 init-payload delivery is not wired up yet; the payload
                // is dropped here rather than sent on-device.
                self.deploy_wasm_cell_embedded(
                    cell_sri, class_name, runtime, gen_id, lineage, arguments,
                )
                .await
            }
            RuntimeKind::Unknown => {
                let rt = runtime.id();
                bail!(
                    "cannot deploy wasm cell '{cell_sri}' on runtime '{rt}': \
                     no supported deploy path for this runtime"
                )
            }
        }
    }
}
