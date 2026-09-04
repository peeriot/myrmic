//! Crate for functionality shared between different self-organization plugins

pub(crate) mod ble;
pub(crate) mod capabilities;
pub(crate) mod cells;
pub(crate) mod configs;
pub(crate) mod consts;
pub(crate) mod error;
pub(crate) mod event_loop;
pub mod exec_registry;
pub(crate) mod fs;
pub mod gateway_config;
pub(crate) mod mqtt;
pub mod node_lease;
pub(crate) mod records;
pub(crate) mod reference;
pub(crate) mod serde;
pub mod supervision;
pub(crate) mod topics;
pub(crate) mod types;
pub(crate) mod utils;

pub use db_client::v1::{Client as DbClient, models::TxId, models::tx_begin, models::tx_commit};

pub use ble::BleAddress;
pub use capabilities::{TagRequirement, check_tag_requirements};
pub use cell_mailbox::{Mailbox, OutgoingMessage};
pub use cell_protocol::{CapabilityTag, ExecRuntimeInfo, ExecutionCapabilities};
pub use cells::CMD_DEPLOY;
pub use cells::cell_lost::emit_cell_lost;
pub use cells::cell_lost::report_cell_death;
pub use cells::class_registry;
pub use cells::commands::CellCommandOutcome;
pub use cells::deployment_error::{
    ArtifactKind, CellFailure, CellFailureKind, CellInfeasibility, DeploymentError,
    RejectionReason, RuntimeRejection,
};
pub use cells::instance_registry;
pub use cells::lifecycle::{
    CellUndeployRequest, SpawnLineage, WasmCellDeployRequest, delete_application, deploy_cells,
    deploy_http_bridge, deploy_mqtt_bridge, deploy_wasm_cell, undeploy_cell,
};
pub use cells::placement::{
    PlacementClaimOutcome, claim_placement, commit_placement, ensure_placement_exists,
    ensure_placement_exists_in_tx, get_placement, list_placements, list_placements_in_tx,
    placement_exists, placement_exists_in_tx, remove_placement, remove_placement_with_db,
};
pub use cells::root_death;
pub use cells::root_restart;
pub use cells::spawn_gate;
pub use configs::ExecConfig;
pub use consts::*;
pub use error::{Error, Result, is_query_timeout, query_err_payload};
pub use event_loop::client::Client;
pub use event_loop::queryables::{QueryableTrait, set_up_queryable};
pub use fs::{FilestorePath, MissingFileRecord, WritableDirectory};
pub use mqtt::{
    BrokerAddress as MqttBrokerAddress, MqttConnection, PubTopic as MqttPubTopic, Qos as MqttQos,
    SubTopic as MqttSubTopic,
};
pub use myrmic_common::cells::{CellLost, LostReason, SYS_CELL_LOST, SYS_COMMAND_PREFIX};
pub use records::OrchRuntimeRecord;
pub use records::app_deployment::{
    BodyTemplate, CellConfig, CellDeployment, DeployRequest, HttpBridgeApi, HttpBridgeConfig,
    HttpBridgeRecord, MqttBridge, MqttBridgeConfig, MqttBridgeDef, MqttBridgeRecord,
    RequirementTags, ResponseHeaderTemplate, RestartPolicy, RestartType, TemplateSegment,
    TemplateSegments, WireHttpEndpoint, WireHttpRequestTemplate, WireHttpResponseTemplate,
    WireHttpResponseVariant, WireMqttEgress, WireMqttIngress, should_restart, status_variant_name,
};
pub use records::tasks::connectors::{InputRecord, OutputRecord};
pub use reference::identifiers::{AsDeploymentIdentifier, DeploymentIdentifier, RuntimeIdentifier};
pub use reference::ids::{DeploymentId, PortId, TaskId};
pub use serde::SorgPayload;
pub use topics::*;
pub use types::*;
pub use utils::{
    blob_link, blob_move, blob_resolve, blob_store, blob_unlink, find_measurement, key_delete,
    key_get, key_prefix, key_put, path_resolve, paths_list, publish_measurement,
    query_exec_runtimes, query_orch_runtimes, sem_select, sem_update, tb_count, tb_delete, tb_get,
    tb_insert, tb_list,
};

pub type ZenohError = Box<dyn std::error::Error + Send + Sync + 'static>;

pub(crate) use event_loop::client::ClientSendError;
