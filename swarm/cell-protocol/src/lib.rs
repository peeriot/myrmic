//! Wire-protocol types shared between the **sorg execution layer** (OS) and the **cell host**
//! running on embedded targets (like the ESP32).
//!
//! # Purpose
//!
//! Cells communicate with the rest of the swarm via Zenoh messages. In many cases there is the need
//! of serializing/deserializing data (like query attachments) so that all components understand it.
#![cfg_attr(target_os = "none", no_std)]
#![warn(missing_docs)]
#![deny(missing_debug_implementations)]

#[cfg(target_os = "none")]
extern crate alloc as sys;

use core::fmt::Display;
use core::str::FromStr;
#[cfg(not(target_os = "none"))]
use std as sys;

mod exec_runtime;
pub use exec_runtime::{CapabilityTag, ExecRuntimeInfo, ExecutionCapabilities, RuntimeKind};
pub mod node_tags;
pub mod replication;
pub mod supervision;
mod watchdog;
pub use watchdog::{WatchdogResetReason, WatchdogResetReport};

use db_commons::NAMESPACE_SORG;
pub use db_commons::models::BlobHash;
use db_commons::models::Scope;
use myrmic_common::cells::{
    Command, ERR_CELL_CMD_CELL_ERROR, ERR_CELL_CMD_CELL_NOT_PRESENT,
    ERR_CELL_CMD_COMMAND_NOT_PRESENT, ERR_CELL_CMD_INTERNAL, ERR_CELL_CMD_TIMEOUT, Event,
};
#[cfg(feature = "opentelemetry")]
use opentelemetry::trace::{SpanContext, TraceState};
#[cfg(feature = "opentelemetry")]
use opentelemetry::{SpanId, TraceFlags, TraceId};
use serde::{Deserialize, Serialize};
use sys::borrow::ToOwned;
use sys::string::ToString;
use sys::{string::String, vec::Vec};
use uuid::Uuid;
// The deterministic SRN <-> SRI derivation, re-exported so the host and CLI
// resolve names through the exact same primitives the guest SDK uses.
pub use myrmic_common::cells::naming;
pub use myrmic_common::cells::{NameError, Sri, Srn, child_sri, resolve_target, sri_of_path};
// One definition of where gateway data lives, shared by the guest SDK, the
// host, and the db plugin that replicates it.
pub use myrmic_common::gateway::{ASSETS_DB as GATEWAY_ASSETS_DB, NAMESPACE_GATEWAY};
#[cfg(not(target_os = "none"))]
use zenoh::config::ZenohId;
use zenoh_protocol::core::ZenohIdProto;

/// Database of cell placements
pub const PLACEMENT_DB: &str = "cell-placement";
/// Table of the placement entries
pub const PLACEMENT_TABLE: &str = "entries";

/// Returns the DB scope for cell placements.
pub fn placement_scope() -> Scope {
    Scope::new(NAMESPACE_SORG, PLACEMENT_DB, "p")
}

const CLASS_REGISTRY_DB: &str = "cell-class-registry";
/// Table of the cell classes in the registry
pub const CLASS_REGISTRY_TABLE: &str = "entries";

const INSTANCE_REGISTRY_DB: &str = "cell-instance-registry";
/// Table of the cell instances in the registry
pub const INSTANCE_REGISTRY_TABLE: &str = "entries";

/// Returns the DB scope for the cell class registry.
pub fn class_registry_scope() -> Scope {
    Scope::new(NAMESPACE_SORG, CLASS_REGISTRY_DB, "p")
}

/// Returns the DB scope for the cell instance registry.
pub fn instance_registry_scope() -> Scope {
    Scope::new(NAMESPACE_SORG, INSTANCE_REGISTRY_DB, "p")
}

const ROOT_RESTART_DB: &str = "root-restart";
/// Table of per-root restart specs, keyed by SRI. Written by the orchestrator
/// at deploy for roots with an enabled restart policy; drives auto-restart.
pub const ROOT_RESTART_TABLE: &str = "entries";

/// Returns the DB scope for the root restart registry.
pub fn root_restart_scope() -> Scope {
    Scope::new(NAMESPACE_SORG, ROOT_RESTART_DB, "p")
}

const ROOT_DEATH_DB: &str = "root-death";
/// Table of pending root deaths, keyed by SRI. An exec writes one when a root
/// dies on a live node (carrying the true `LostReason`); the orchestrator
/// consumes it to drive a restart decision, then deletes it. Transient.
pub const ROOT_DEATH_TABLE: &str = "entries";

/// Returns the DB scope for the pending root-death signals.
pub fn root_death_scope() -> Scope {
    Scope::new(NAMESPACE_SORG, ROOT_DEATH_DB, "p")
}

/// Database of the gateway (socket-routing) config registry
const GATEWAY_CONFIG_DB: &str = "gateway-config";
/// Table of the gateway config entries in the registry
pub const GATEWAY_CONFIG_TABLE: &str = "entries";

/// Returns the DB scope for the gateway (socket-routing) config registry.
///
/// Web applications register how they should be served (URL, static assets,
/// cell API) here on deploy; every `myrmic gateway` process discovers and
/// watches this scope. Lives in the gateway namespace alongside the assets it
/// points at, replicated by every node so all gateways see the same routing
/// table.
pub fn gateway_config_scope() -> Scope {
    Scope::new(NAMESPACE_GATEWAY, GATEWAY_CONFIG_DB, "p")
}

/// Database of the exec registry
pub const EXEC_REGISTRY_DB: &str = "exec-registry";
/// Database of the exec deployment
pub const EXEC_DEPLOYMENT_DB: &str = "exec-deployment";
/// Table of the exec runtimes in the registry
pub const EXEC_REGISTRY_TABLE: &str = "entries";
/// Database of exec runtime watchdog reports
pub const EXEC_WATCHDOG_DB: &str = "exec-watchdog";
/// Table of hardware-watchdog reset reports, keyed by device id
pub const WATCHDOG_RESETS_TABLE: &str = "resets";
/// Database of node liveness leases
pub const NODE_LEASE_DB: &str = "node-lease";
/// Table of per-node lease rows, keyed by runtime id
pub const NODE_LEASE_TABLE: &str = "entries";

/// Namespace under which per-cell keys are stored in the datalayer.
pub const NAMESPACE_CELLS: &str = "CELLS";

/// Table for deployment messages
pub const DEPLOYMENT_TABLE: &str = "deployments";
/// Table storing deployment responses
pub const DEPLOYMENT_RESPONSES_TABLE: &str = "responses";
/// Table storing messages
pub const MESSAGES_TABLE: &str = "messages";
/// Table storing events
pub const EVENTS_TABLE: &str = "events";
/// Table storing the discarded errored entries
pub const DEADLETTER_TABLE: &str = "errors";

/// A target architecture that produces AOT + meta artifact pairs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ArtifactPlatform {
    /// Espressif riscv32imac target (e.g ESP32-C5, ESP32-C6, ESP32-C61)
    Riscv32imac,
}

impl ArtifactPlatform {
    /// Returns the canonical string representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Riscv32imac => "riscv32imac",
        }
    }
}

impl Display for ArtifactPlatform {
    fn fmt(&self, f: &mut sys::fmt::Formatter<'_>) -> sys::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when parsing an unknown artifact-platform string.
#[derive(Debug)]
pub struct UnknownArtifactPlatform;

impl Display for UnknownArtifactPlatform {
    fn fmt(&self, f: &mut sys::fmt::Formatter<'_>) -> sys::fmt::Result {
        f.write_str("unknown artifact platform")
    }
}

#[cfg(not(target_os = "none"))]
impl std::error::Error for UnknownArtifactPlatform {}

impl FromStr for ArtifactPlatform {
    type Err = UnknownArtifactPlatform;

    fn from_str(s: &str) -> core::result::Result<Self, Self::Err> {
        // This contains also chips so it's backwards compatible
        match s {
            "riscv32imac" | "esp32c5" | "esp32_c5" | "esp32c6" | "esp32_c6" | "esp32c61"
            | "esp32_c61" => Ok(Self::Riscv32imac),
            _ => Err(UnknownArtifactPlatform),
        }
    }
}

/// Information about a registered cell class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassInfo {
    /// The name under which the class is registered.
    pub name: String,
    /// The SHA-256 content hash of the Wasm binary, if one has been uploaded.
    pub wasm_hash: Option<BlobHash>,
    /// Target-specific artifacts registered for this class.
    pub artifacts: Vec<ArtifactInfo>,
}

/// Generation of a cell instance: a uhlc timestamp minted at deploy
/// admission — the same hybrid-logical-clock ordering the db uses for its
/// writes. Unique (node-id component), totally ordered (time-major, node id
/// as tiebreak), and causal through the messaging layer: a node that has
/// observed a previous life mints a strictly greater generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Gen {
    time: u64,
    id: u128,
}

impl Gen {
    /// Captures a freshly minted HLC timestamp as a generation.
    #[cfg(not(target_os = "none"))]
    pub fn from_timestamp(ts: &uhlc::Timestamp) -> Self {
        Self {
            time: ts.get_time().0,
            id: u128::from_le_bytes(ts.get_id().to_le_bytes()),
        }
    }

    /// For tests and tooling. `id` must be nonzero for
    /// [`to_timestamp`](Self::to_timestamp) to succeed.
    pub const fn from_parts(time: u64, id: u128) -> Self {
        Self { time, id }
    }

    /// Recovers the uhlc timestamp; `None` if the id bytes are zero
    /// (corrupt row).
    #[cfg(not(target_os = "none"))]
    pub fn to_timestamp(&self) -> Option<uhlc::Timestamp> {
        Some(uhlc::Timestamp::new(
            uhlc::NTP64(self.time),
            uhlc::ID::try_from(self.id).ok()?,
        ))
    }
}

impl Display for Gen {
    fn fmt(&self, f: &mut sys::fmt::Formatter<'_>) -> sys::fmt::Result {
        write!(f, "{}/{:x}", self.time, self.id)
    }
}

/// Spawn-edge metadata riding a deploy and persisted with the instance: who
/// spawned the cell, that parent's generation, lifetime detachment, the
/// spawn-time local name, and the edge's fencing grace. Defaults describe an
/// external (root) deploy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnLineage {
    /// The SRI of the spawning cell; `None` for roots.
    pub parent: Option<Sri>,
    /// Generation of the spawning parent — the child's fencing anchor.
    /// `None` when the spawner's generation is unknown (external deploys
    /// declaring a parent edge).
    pub parent_gen_id: Option<Gen>,
    /// Lifetime decoupled from the parent (no fencing, no cascade, no
    /// cell lost event). Declared by the parent at spawn.
    pub detached: bool,
    /// Spawn-time local name. SRIs are one-way hashes, so this is the only
    /// place the name survives; `None` for external deploys.
    pub local_name: Option<String>,
    /// Per-edge fencing tolerance: how long this cell outlives its parent's
    /// silence before fencing kills it. `None` = the cluster default TTL.
    pub grace_ms: Option<u64>,
    /// Per-edge death deadline: how long this cell's node must be silent
    /// before observers declare the cell dead (rows released, parent told).
    /// Short = fast failover but misfires on slow reboots; long lets the
    /// cell ride one out. Clamped to at least twice the lease renewal
    /// period; `None` = the cluster default (TTL + margin).
    pub deadline_ms: Option<u64>,
}

/// A cell instance: one *life* of an SRI. The row is keyed by the SRI and
/// stores the full struct (sri included) as its value. Postcard-serialized
/// identically by the sorg execution layer and the embedded cell host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellInstance {
    /// The SRI identifying this instance.
    pub sri: Sri,
    /// The name of the class this instance was created from.
    pub class_name: String,
    /// Generation: a fresh stamp per deploy of this SRI. Same name, new
    /// life, new generation — staleness decisions compare generations.
    pub gen_id: Gen,
    /// The spawn edge this instance was born on.
    pub lineage: SpawnLineage,
}

/// Information about a target-specific artifact pair (aot + meta).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactInfo {
    /// The target architecture.
    pub platform: ArtifactPlatform,
    /// The SHA-256 content hash of the AOT binary.
    pub aot_hash: BlobHash,
    /// The SHA-256 content hash of the meta file.
    pub meta_hash: BlobHash,
}

/// An artifact to be added to a cell class.
#[derive(Debug, Clone)]
pub enum ClassArtifact {
    /// A Wasm binary (canonical class artifact).
    Wasm(Vec<u8>),
    /// A target-specific AOT + meta pair.
    Aot {
        /// The target architecture.
        platform: ArtifactPlatform,
        /// The AOT-compiled binary.
        aot_blob: Vec<u8>,
        /// The metadata file.
        meta_blob: Vec<u8>,
    },
}

/// Controls whether an operation adding sth to the datalayer should overwrite existing data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddMode {
    /// Reject the operation if data with the same reference exists.
    Strict,
    /// Overwrite existing data (unless this violates other, operation-specific constraints).
    Force,
}

/// A cell's placement — where it currently lives — stored in the datalayer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementEntry {
    /// The cell's self-referential identifier.
    pub sri: Sri,
    /// Where the cell is placed (WASM runtime, bridge, or placeholder).
    pub kind: PlacementKind,
    /// The application this cell belongs to, if any.
    pub app: Option<String>,
    /// Generation of the deployed instance. The placement row is the
    /// liveness anchor for that generation: a running cell whose row
    /// carries a different generation has been superseded.
    pub gen_id: Gen,
}

/// Where a cell is placed: a WASM exec runtime, a native bridge, or nowhere yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlacementKind {
    /// A WASM cell running on an execution runtime.
    Wasm {
        /// The runtime this cell is loaded on, including its capabilities.
        runtime: ExecRuntimeInfo,
    },
    /// A bridge cell (HTTP or MQTT) running natively, addressed by its own SRI.
    Bridge {
        /// The SRI this bridge cell's mailbox is registered under.
        sri: Sri,
    },
    /// Transient state: SRI has been claimed but the cell is not yet deployed.
    Placeholder,
}

/// The ID of a self-organization runtime (equivalent to the ID of the zenoh runtime that we are running on)
/// Runtime IDs are unique within the system
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone, Copy, Hash)]
pub struct RuntimeId(ZenohIdProto);

impl Display for RuntimeId {
    fn fmt(&self, f: &mut sys::fmt::Formatter<'_>) -> sys::fmt::Result {
        write!(f, "{rt_str}", rt_str = self.0)
    }
}

impl From<ZenohIdProto> for RuntimeId {
    fn from(proto_id: ZenohIdProto) -> Self {
        RuntimeId(proto_id)
    }
}

#[cfg(not(target_os = "none"))]
impl From<ZenohId> for RuntimeId {
    fn from(id: ZenohId) -> Self {
        RuntimeId(id.into())
    }
}

impl FromStr for RuntimeId {
    type Err = <ZenohIdProto as FromStr>::Err;

    fn from_str(s: &str) -> core::result::Result<Self, Self::Err> {
        ZenohIdProto::from_str(s).map(RuntimeId)
    }
}

/// A command delivered to a cell's mailbox, including its serialized payload and attachment metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailboxCommand {
    /// The command variant to execute.
    pub cmd: Command,
    /// Optional serialized arguments for the command.
    pub payload: Option<Vec<u8>>,
    /// Routing and tracing metadata attached to this command.
    pub attachment: CellAttachment,
}

/// A message that could not be delivered or processed, preserved for inspection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadLetter {
    /// Human-readable explanation of why the message was dead-lettered.
    pub reason: String,
    /// The original message content that failed delivery.
    pub ty: DeadLetterType,
}

/// The original content of a dead-lettered message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeadLetterType {
    /// Raw bytes that could not be deserialized into a known message type.
    Payload(Vec<u8>),
    /// A well-formed command that failed to be delivered or processed.
    Command(MailboxCommand),
}

/// An event delivered to a cell's mailbox, including its serialized payload and attachment metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailboxEvent {
    /// The event variant being delivered.
    pub event: Event,
    /// Serialized event data.
    pub payload: Vec<u8>,
    /// Routing and tracing metadata attached to this event.
    pub attachment: CellAttachment,
}

/// A [`CellAttachment`] can be used to attach multiple different "attachments".
/// To keep it extensible, all attached data is optional.
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct CellAttachment {
    /// `OpenTelemetry` span context for distributed tracing across components.
    #[serde(default)]
    pub span_context: Option<SerializableSpanContext>,
    /// Identity of the cell that emitted this message. Stamped host-side from
    /// the sender's verified identity; `None` for messages that originate
    /// outside a cell (e.g. the CLI or a gateway).
    #[serde(default)]
    sender: Option<Uuid>,
}

/// Minimal, serializable representation of an `OpenTelemetry` span context.
/// Used for embedding trace metadata into messages exchanged between components.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializableSpanContext {
    trace_id: [u8; 16],
    span_id: [u8; 8],
    trace_flags: u8,
}

impl SerializableSpanContext {
    /// return the trace id as u128
    pub fn trace_id(&self) -> u128 {
        u128::from_be_bytes(self.trace_id)
    }
}

#[cfg(feature = "opentelemetry")]
impl From<SpanContext> for SerializableSpanContext {
    fn from(value: SpanContext) -> Self {
        Self {
            trace_id: value.trace_id().to_bytes(),
            span_id: value.span_id().to_bytes(),
            trace_flags: value.trace_flags().to_u8(),
        }
    }
}

#[cfg(feature = "opentelemetry")]
impl From<SerializableSpanContext> for SpanContext {
    fn from(value: SerializableSpanContext) -> Self {
        let SerializableSpanContext {
            trace_id,
            span_id,
            trace_flags,
        } = value;

        SpanContext::new(
            TraceId::from_bytes(trace_id),
            SpanId::from_bytes(span_id),
            TraceFlags::new(trace_flags),
            true,
            TraceState::default(),
        )
    }
}

// the from `(u128, u64)` to `SerializableSpanContext` conversion is only used in cell-ctl to
// create an initial trace ID that can be printed as part of issuing commands via cell-ctl. we
// need to set `trace_flags` to `SAMPLED` otherwise all children will be hidden.
#[cfg(feature = "opentelemetry")]
impl From<(u128, u64)> for SerializableSpanContext {
    fn from((trace_id, span_id): (u128, u64)) -> Self {
        SerializableSpanContext {
            trace_id: trace_id.to_be_bytes(),
            span_id: span_id.to_be_bytes(),
            trace_flags: TraceFlags::SAMPLED.to_u8(),
        }
    }
}

impl CellAttachment {
    /// Returns the sender identity, if present.
    #[must_use]
    pub fn sender(&self) -> Option<Uuid> {
        self.sender
    }

    /// Sets the sender identity in place.
    pub fn set_sender(&mut self, sender: Option<Uuid>) {
        self.sender = sender;
    }

    /// Returns the span context, if present.
    #[cfg(feature = "opentelemetry")]
    pub fn span_context(&self) -> Option<SpanContext> {
        self.span_context.clone().map(SerializableSpanContext::into)
    }

    /// Builder that takes a span context
    #[cfg(feature = "opentelemetry")]
    pub fn with_span_context<C>(self, span_context: Option<C>) -> Self
    where
        C: Into<SerializableSpanContext>,
    {
        Self {
            span_context: span_context.map(Into::into),
            ..self
        }
    }
}

#[cfg(feature = "opentelemetry")]
impl From<Option<SpanContext>> for CellAttachment {
    fn from(value: Option<SpanContext>) -> Self {
        Self::default().with_span_context(value)
    }
}

/// A Cell deployment command
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeploymentCommand {
    /// Deploys the cell. If any existing running cell is present, it will be terminated before the
    /// new cell is deployed.
    Deploy {
        /// Name of the cell class to deploy; the firmware derives the artifact
        /// paths from this using [`ArtifactLocation`] and its own target.
        class: String,
        /// SRI of the deployed cell
        sri: Sri,
        /// The arguments to be provided during the initialistation phase.
        payload: Option<Vec<u8>>,
        /// Generation minted for this deploy: the body's fencing identity.
        gen_id: Gen,
        /// The spawn edge the cell is born on; the host stores it per cell
        /// and runs the fencing checks from it (see [`supervision`]).
        lineage: SpawnLineage,
    },
    /// Deletes the running cell (if any)
    Delete {
        /// SRI of the cell to delete
        sri: Sri,
    },
}

/// A deployment confirmation that comes as a response after a [`DeploymentCommand`] has been
/// processed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeploymentConfirmation {
    /// Confirms the result of a deployment command
    Deployed {
        /// Whether an error is present
        ///
        /// * `Some(_)`: Deployment failed
        /// * `None`: Deployment successful
        failure: Option<String>,
        /// SRI of the deployed cell
        sri: Sri,
    },
    /// Confirms Cell deletion
    Deleted {
        /// SRI of the deleted cell
        sri: Sri,
    },
}

/// Cell command error that gets shared on callbacks
#[derive(Debug, Serialize, Deserialize, thiserror::Error)]
pub enum CellCommandError {
    #[error("no response received within the timeout")]
    #[allow(missing_docs)]
    Timeout,
    #[error("cell has no placement entry")]
    #[allow(missing_docs)]
    CellNotPresent,
    #[error("command not found on the cell")]
    #[allow(missing_docs)]
    CommandNotPresent,
    #[error("cell error: {0}")]
    #[allow(missing_docs)]
    CellError(String),
    #[error("internal framework error")]
    #[allow(missing_docs)]
    Internal,
}

impl From<CellCommandError> for i32 {
    fn from(value: CellCommandError) -> Self {
        match value {
            CellCommandError::Timeout => ERR_CELL_CMD_TIMEOUT,
            CellCommandError::CellNotPresent => ERR_CELL_CMD_CELL_NOT_PRESENT,
            CellCommandError::CommandNotPresent => ERR_CELL_CMD_COMMAND_NOT_PRESENT,
            CellCommandError::CellError(_) => ERR_CELL_CMD_CELL_ERROR,
            CellCommandError::Internal => ERR_CELL_CMD_INTERNAL,
        }
    }
}

/// Where a class artifact is stored in the datalayer: its scope and blob path.
///
/// The single source of truth for class-artifact locations — the class registry
/// writes through it and the execution runtimes (linux and embedded) read
/// through it, so every side resolves the same blob.
#[derive(Debug, Clone)]
pub struct ArtifactLocation {
    scope: Scope,
    path: String,
}

impl ArtifactLocation {
    /// Location of a class's wasm binary (target-independent).
    #[must_use]
    pub fn wasm(class_name: &str) -> Self {
        Self {
            scope: class_registry_scope(),
            path: sys::format!("/{class_name}/wasm"),
        }
    }

    /// Location of a class's AOT binary built for `target`.
    #[must_use]
    pub fn aot(class_name: &str, target: ArtifactPlatform) -> Self {
        Self {
            scope: class_registry_scope(),
            path: sys::format!("/{class_name}/{}/aot", target.as_str()),
        }
    }

    /// Location of a class's metadata file built for `target`.
    #[must_use]
    pub fn meta(class_name: &str, target: ArtifactPlatform) -> Self {
        Self {
            scope: class_registry_scope(),
            path: sys::format!("/{class_name}/{}/meta", target.as_str()),
        }
    }

    /// The datalayer scope the artifact lives in.
    #[must_use]
    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    /// The blob path within the scope.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Consumes into the `(scope, path)` pair a blob / path-resolve request takes.
    #[must_use]
    pub fn into_parts(self) -> (Scope, String) {
        (self.scope, self.path)
    }
}

/// Creates a scope that targets the give Cell SRI
pub fn scope_of_cell(sri: impl Display) -> Scope {
    Scope {
        namespace: NAMESPACE_CELLS.to_owned(),
        database: sri.to_string(),
        ..Default::default()
    }
}

/// Creates a scope that targets events
pub fn scope_of_event(event: impl Display) -> Scope {
    Scope {
        namespace: NAMESPACE_CELLS.to_owned(),
        database: String::from("@events"),
        schema: event.to_string(),
    }
}

/// Creates the scope of the exec registry
pub fn scope_of_exec_registry() -> Scope {
    Scope::new(NAMESPACE_SORG, EXEC_REGISTRY_DB, "p")
}

/// Creates the scope of the watchdog reset reports
pub fn scope_of_watchdog_resets() -> Scope {
    Scope::new(NAMESPACE_SORG, EXEC_WATCHDOG_DB, "p")
}

/// Creates the scope of the exec deployment
pub fn scope_of_deployment(exec_id: impl Display) -> Scope {
    Scope::new(NAMESPACE_SORG, EXEC_DEPLOYMENT_DB, exec_id.to_string())
}

/// Creates the scope of the node liveness leases
pub fn node_lease_scope() -> Scope {
    Scope::new(NAMESPACE_SORG, NODE_LEASE_DB, "p")
}

/// One row per running node. `seq` strictly increases while the node lives;
/// observers measure staleness from the last advance they saw, on their own
/// monotonic clock. Row timestamps are never compared across nodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeLease {
    /// Stable hardware/device identity (survives reboots; a `RuntimeId` does not).
    pub device_id: String,
    /// Renewal counter.
    pub seq: u64,
    /// Silence observers tolerate before declaring this node dead, declared
    /// by the writer to fit its own renewal cadence (a radio-constrained node
    /// renews slower and declares a larger ttl).
    pub ttl_ms: u64,
}

#[cfg(all(test, feature = "opentelemetry"))]
mod tests {
    use opentelemetry::trace::{SpanContext, SpanId, TraceFlags, TraceId, TraceState};

    use super::CellAttachment;

    fn make_span_context() -> SpanContext {
        SpanContext::new(
            TraceId::from_bytes([
                0x4b, 0xf9, 0x2f, 0x35, 0x77, 0xb3, 0x4d, 0xa6, 0xa3, 0xce, 0x92, 0x9d, 0x0e, 0x0e,
                0x47, 0x36,
            ]),
            SpanId::from_bytes([0x00, 0xf0, 0x67, 0xaa, 0x0b, 0xa9, 0x02, 0xb7]),
            TraceFlags::SAMPLED,
            true,
            TraceState::default(),
        )
    }

    #[test]
    fn full_cell_attachment_roundtrips() {
        let span_ctx = make_span_context();
        let attachment = CellAttachment::default().with_span_context(Some(span_ctx.clone()));
        let bytes = postcard::to_allocvec(&attachment).expect("serialization failed");
        let deserialized: CellAttachment =
            postcard::from_bytes(&bytes).expect("deserialization failed");

        let recovered = deserialized
            .span_context()
            .expect("span context missing after roundtrip");
        assert_eq!(recovered.trace_id(), span_ctx.trace_id());
        assert_eq!(recovered.span_id(), span_ctx.span_id());
        assert_eq!(recovered.trace_flags(), span_ctx.trace_flags());
    }
}

#[cfg(test)]
mod instance_row_tests {
    use super::*;

    fn sri() -> Sri {
        sri_of_path("row-test-app").unwrap().into()
    }

    #[test]
    fn cell_instance_round_trips() {
        let v = CellInstance {
            sri: sri(),
            class_name: "c".into(),
            gen_id: Gen::from_parts(7, 1),
            lineage: SpawnLineage {
                parent: None,
                parent_gen_id: Some(Gen::from_parts(9, 1)),
                detached: true,
                local_name: Some("pump".into()),
                grace_ms: Some(30_000),
                deadline_ms: Some(120_000),
            },
        };
        let bytes = postcard::to_allocvec(&v).unwrap();
        assert_eq!(postcard::from_bytes::<CellInstance>(&bytes).unwrap(), v);
    }

    #[test]
    fn placement_entry_round_trips() {
        let v = PlacementEntry {
            sri: sri(),
            kind: PlacementKind::Placeholder,
            app: None,
            gen_id: Gen::from_parts(3, 1),
        };
        let bytes = postcard::to_allocvec(&v).unwrap();
        assert_eq!(postcard::from_bytes::<PlacementEntry>(&bytes).unwrap(), v);
    }
}

#[cfg(test)]
mod node_lease_tests {
    use core::str::FromStr;

    use super::{NodeLease, RuntimeId};

    #[test]
    fn node_lease_postcard_round_trip() {
        let lease = NodeLease {
            device_id: "dev-42".into(),
            seq: 17,
            ttl_ms: 45_000,
        };
        let bytes = postcard::to_allocvec(&lease).unwrap();
        let back: NodeLease = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, lease);
    }

    #[test]
    fn runtime_id_round_trips_through_display() {
        let id: RuntimeId = zenoh_protocol::core::ZenohIdProto::try_from(&[7u8; 8][..])
            .unwrap()
            .into();
        let parsed = RuntimeId::from_str(&id.to_string()).unwrap();
        assert_eq!(parsed, id);
    }
}
