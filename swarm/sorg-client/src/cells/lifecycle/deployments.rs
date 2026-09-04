use cell_protocol::{PlacementEntry, Sri};
use sorg_common::{
    DeployRequest, DeploymentError, HttpBridgeApi, MqttBridge, RequirementTags, SpawnLineage,
    delete_application, deploy_cells, deploy_wasm_cell, get_placement, list_placements,
    placement_exists, undeploy_cell,
};

use crate::{Client, Result};

impl Client {
    /// Returns the placements of all currently placed cells.
    pub async fn list_placements(&self) -> Result<Vec<PlacementEntry>> {
        Ok(list_placements(self.session()).await?)
    }

    /// Returns the placement of a cell, or `None` if it has none.
    pub async fn get_placement(&self, sri: &Sri) -> Result<Option<PlacementEntry>> {
        Ok(get_placement(self.session(), sri).await?)
    }

    /// Checks whether the cell with the given SRI has a placement.
    pub async fn placement_exists(&self, sri: &Sri) -> Result<bool> {
        Ok(placement_exists(self.session(), sri).await?)
    }

    /// Deploys a cell by SRI. The instance must already exist in the
    /// instance registry; the class name is looked up from there.
    pub async fn deploy_cell(&self, cell_sri: Sri, tags: RequirementTags) -> Result<()> {
        let instance = self.inspect_instance(&cell_sri).await?;
        self.deploy_wasm_cell(cell_sri, &instance.class_name, tags)
            .await
            .map_err(sorg_common::Error::custom)?;
        Ok(())
    }

    /// Deploys a wasm cell onto an execution runtime matching the given tag
    /// requirements. The cell class must already be registered in the
    /// datalayer.
    pub async fn deploy_wasm_cell(
        &self,
        cell_sri: Sri,
        class_name: &str,
        tags: RequirementTags,
    ) -> std::result::Result<(), DeploymentError> {
        self.deploy_wasm_cell_with_arguments(cell_sri, class_name, tags, None, None)
            .await
    }

    /// Like [`deploy_wasm_cell`](Self::deploy_wasm_cell), but delivers
    /// `arguments` to the cell's `#[init]` as its argument buffer — the root /
    /// CLI counterpart to a spawner's `spawn_with`. A cell whose init takes no
    /// payload rejects non-empty arguments. `app` names the app this root
    /// introduces (typically its own SRN); `None` leaves it ungrouped.
    pub async fn deploy_wasm_cell_with_arguments(
        &self,
        cell_sri: Sri,
        class_name: &str,
        tags: RequirementTags,
        arguments: Option<Vec<u8>>,
        app: Option<String>,
    ) -> std::result::Result<(), DeploymentError> {
        deploy_wasm_cell(
            self.session(),
            cell_sri,
            class_name,
            tags,
            self.config.query_timeout(),
            // Cells deployed through the client are roots (CLI / app); the
            // lineage is threaded host-side for spawned cells, not here.
            SpawnLineage::default(),
            arguments,
            app,
        )
        .await
    }

    /// Deploys an MQTT bridge cell.
    pub async fn deploy_mqtt_bridge(
        &self,
        cell_sri: Sri,
        bridge: MqttBridge,
        tags: RequirementTags,
    ) -> std::result::Result<(), DeploymentError> {
        sorg_common::deploy_mqtt_bridge(
            self.session(),
            cell_sri,
            bridge,
            tags,
            self.config.query_timeout(),
        )
        .await
    }

    /// Deploys an HTTP bridge cell.
    pub async fn deploy_http_bridge(
        &self,
        cell_sri: Sri,
        api: HttpBridgeApi,
        tags: RequirementTags,
    ) -> std::result::Result<(), DeploymentError> {
        sorg_common::deploy_http_bridge(
            self.session(),
            cell_sri,
            api,
            tags,
            self.config.query_timeout(),
        )
        .await
    }

    /// Undeploys a cell from the system.
    pub async fn undeploy_cell(&self, cell_sri: Sri) -> Result<()> {
        Ok(undeploy_cell(self.session(), cell_sri, self.config.query_timeout()).await?)
    }

    /// Deploys a batch of cells atomically via the orchestration plugin.
    pub async fn deploy_cells(
        &self,
        request: DeployRequest,
    ) -> std::result::Result<(), DeploymentError> {
        deploy_cells(self.session(), request, self.config.query_timeout()).await
    }

    /// Deletes every cell that shares the given app name.
    pub async fn delete_application(&self, name: &str) -> Result<()> {
        Ok(delete_application(self.session(), name, self.config.query_timeout()).await?)
    }

    /// Returns the SRIs of every deployed cell that shares the given app name.
    /// Empty if nothing carries that name.
    pub async fn app_members(&self, name: &str) -> Result<Vec<Sri>> {
        Ok(list_placements(self.session())
            .await?
            .into_iter()
            .filter(|cell| cell.app.as_deref() == Some(name))
            .map(|cell| cell.sri)
            .collect())
    }

    /// Returns the gateway route mounts owned by the given cell (empty if none).
    pub async fn cell_routes(&self, sri: &Sri) -> Result<Vec<String>> {
        Ok(
            sorg_common::gateway_config::list_gateway_routes(self.session())
                .await?
                .into_iter()
                .filter(|route| route.owner == *sri)
                .map(|route| route.mount)
                .collect(),
        )
    }
}
