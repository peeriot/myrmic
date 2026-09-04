//! Cell lifecycle wire protocol and client-side helpers.
//!
//! Two concerns share this module because they are the two ends of the same
//! wire:
//!
//! - [`CellDeployment`] / [`CellUndeployRequest`] are the payload DTOs that
//!   travel on `TOPIC_ORCH_CELL_DEPLOY` / `TOPIC_ORCH_CELL_UNDEPLOY` and are
//!   deserialized by the orchestration plugin's handlers.
//! - [`deploy_cell`] / [`undeploy_cell`] are the sender side, used by clients
//!   (e.g. `sorg-client`) to issue the request and await the reply.

use std::time::Duration;

use cell_protocol::{Gen, Sri};
use serde::{Deserialize, Serialize};
use zenoh::Session;

use crate::{
    CellDeployment, DeployRequest, DeploymentError, RequirementTags, SorgPayload,
    TOPIC_ORCH_APP_DELETE, TOPIC_ORCH_CELL_DEPLOY, TOPIC_ORCH_CELL_UNDEPLOY, bail,
    is_query_timeout, zenoh_err,
};

/// Deserializes a [`DeploymentError`] from an orchestrator error reply, falling
/// back to [`DeploymentError::Internal`] if the payload can't be decoded.
fn deploy_err_from_reply(payload: &zenoh::bytes::ZBytes) -> DeploymentError {
    DeploymentError::from_payload(payload, "deser deployment error from orchestrator")
        .unwrap_or_else(|err| {
            DeploymentError::Internal(format!("failed to deserialize deployment error: {err}"))
        })
}

pub use cell_protocol::SpawnLineage;

/// Request payload for deploying a cell via the execution runtime.
/// Only carries wasm-specific fields — the orchestrator builds this
/// when forwarding a wasm cell deploy to an exec runtime.
#[derive(Debug, Serialize, Deserialize)]
pub struct WasmCellDeployRequest {
    pub cell_sri: Sri,
    pub class_name: String,
    /// Optional payload delivered to the cell's `#[init]` as its argument
    /// buffer. Set by a spawner via `spawn_with`; `None` for root cells.
    pub arguments: Option<Vec<u8>>,
    /// Generation minted for this instance at deploy admission.
    pub gen_id: Gen,
    /// Who spawned this cell (identity + generation), detachment, and the
    /// spawn-time local name. Defaults for root cells.
    pub lineage: SpawnLineage,
}

#[allow(clippy::too_many_arguments)]
async fn deploy_cell(
    session: &Session,
    cell_sri: Sri,
    config: crate::CellConfig,
    tags: RequirementTags,
    timeout: Duration,
    lineage: SpawnLineage,
    arguments: Option<Vec<u8>>,
    app: Option<String>,
) -> std::result::Result<(), DeploymentError> {
    let cell = CellDeployment::new(cell_sri, config)
        .with_tags(tags)
        .with_lineage(lineage)
        .with_arguments(arguments)
        .with_app(app);
    deploy_cells(session, DeployRequest::new(vec![cell]), timeout).await
}

/// Deploys a batch of cells atomically via the orchestration plugin. A single
/// cell (CLI or spawn) is just a batch of one; an app bundle is many. Every
/// cell carries its own [`app`](CellDeployment::app).
pub async fn deploy_cells(
    session: &Session,
    request: DeployRequest,
    timeout: Duration,
) -> std::result::Result<(), DeploymentError> {
    let payload = request
        .to_payload()
        .map_err(|err| DeploymentError::Internal(err.to_string()))?;

    let reply = session
        .get(TOPIC_ORCH_CELL_DEPLOY)
        .payload(payload)
        .timeout(timeout)
        .await
        .map_err(|zen_err| DeploymentError::Internal(format!("cell deploy query: {zen_err}")))?;

    match reply.recv_async().await {
        Ok(reply) => match reply.result() {
            Ok(_sample) => Ok(()),
            Err(err_reply) => {
                if is_query_timeout(err_reply) {
                    return Err(DeploymentError::QueryTimeout);
                }
                Err(deploy_err_from_reply(err_reply.payload()))
            }
        },
        Err(_) => Err(DeploymentError::OrchestratorUnreachable),
    }
}

/// Deploys a wasm cell. The cell class must already be registered in the datalayer.
#[allow(clippy::too_many_arguments)]
pub async fn deploy_wasm_cell(
    session: &Session,
    cell_sri: Sri,
    class_name: &str,
    tags: RequirementTags,
    timeout: Duration,
    lineage: SpawnLineage,
    arguments: Option<Vec<u8>>,
    app: Option<String>,
) -> std::result::Result<(), DeploymentError> {
    deploy_cell(
        session,
        cell_sri,
        crate::CellConfig::Wasm {
            class: class_name.to_owned(),
        },
        tags,
        timeout,
        lineage,
        arguments,
        app,
    )
    .await
}

/// Deploys an MQTT bridge cell.
pub async fn deploy_mqtt_bridge(
    session: &Session,
    cell_sri: Sri,
    bridge: crate::MqttBridge,
    tags: RequirementTags,
    timeout: Duration,
) -> std::result::Result<(), DeploymentError> {
    let app = Some(bridge.cell_name.clone());
    deploy_cell(
        session,
        cell_sri,
        crate::CellConfig::MqttBridge(bridge),
        tags,
        timeout,
        SpawnLineage::default(),
        None,
        app,
    )
    .await
}

/// Deploys an HTTP bridge cell.
pub async fn deploy_http_bridge(
    session: &Session,
    cell_sri: Sri,
    api: crate::HttpBridgeApi,
    tags: RequirementTags,
    timeout: Duration,
) -> std::result::Result<(), DeploymentError> {
    let app = Some(api.cell_name.clone());
    deploy_cell(
        session,
        cell_sri,
        crate::CellConfig::HttpBridge(api),
        tags,
        timeout,
        SpawnLineage::default(),
        None,
        app,
    )
    .await
}

/// Request payload for deleting a cell via the orchestration plugin.
#[derive(Debug, Serialize, Deserialize)]
pub struct CellUndeployRequest {
    pub cell_sri: Sri,
}

/// Undeploys a cell by sending an undeploy request to the orchestration plugin.
pub async fn undeploy_cell(
    session: &Session,
    cell_sri: Sri,
    timeout: Duration,
) -> crate::Result<()> {
    let request = CellUndeployRequest { cell_sri };
    let payload = request.to_payload()?;

    let reply = session
        .get(TOPIC_ORCH_CELL_UNDEPLOY)
        .payload(payload)
        .timeout(timeout)
        .await
        .map_err(|zen_err| zenoh_err!("cell undeploy query", zen_err))?;

    match reply.recv_async().await {
        Ok(reply) => match reply.result() {
            Ok(_sample) => Ok(()),
            Err(err_reply) => {
                let bytes = err_reply.payload().to_bytes();
                let msg = String::from_utf8_lossy(&bytes);
                bail!("failed to undeploy cell: {msg}")
            }
        },
        Err(_) => bail!("no response from an orchestration runtime for cell undeploy request"),
    }
}

/// Deletes an application by sending a delete request to the orchestration plugin.
pub async fn delete_application(
    session: &Session,
    name: &str,
    timeout: Duration,
) -> crate::Result<()> {
    let payload = name.to_owned().to_payload()?;

    let reply = session
        .get(TOPIC_ORCH_APP_DELETE)
        .payload(payload)
        .timeout(timeout)
        .await
        .map_err(|zen_err| zenoh_err!("app delete query", zen_err))?;

    match reply.recv_async().await {
        Ok(reply) => match reply.result() {
            Ok(_sample) => Ok(()),
            Err(err_reply) => {
                let bytes = err_reply.payload().to_bytes();
                let msg = String::from_utf8_lossy(&bytes);
                bail!("failed to delete application: {msg}")
            }
        },
        Err(_) => bail!("no response from an orchestration runtime for app delete request"),
    }
}
