//! Cell identity, messaging, spawning, and lifecycle wire types.

use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

pub use ids::{Sri, Srn};
pub use names::{Command, Event};
pub use naming::{NameError, ROOT_NS, child_sri, resolve_target, sri_of_path, validate_segment};

mod ids;
mod names;
pub mod naming;
pub mod spawn_ref;

// Cell query error codes
/// Command error code: no response within the timeout.
pub const ERR_CELL_CMD_TIMEOUT: i32 = -7;
/// Command error code: the target cell is not present in the system.
pub const ERR_CELL_CMD_CELL_NOT_PRESENT: i32 = -2;
/// Command error code: the cell exists but has no such command, or the
/// arguments don't match what it expects.
pub const ERR_CELL_CMD_COMMAND_NOT_PRESENT: i32 = -3;
/// Command error code: the receiving cell crashed or errored while handling
/// the command.
pub const ERR_CELL_CMD_CELL_ERROR: i32 = -4;
/// Command error code: an internal framework error.
pub const ERR_CELL_CMD_INTERNAL: i32 = -5;
/// Command error code: the provided buffer was too small for the response.
pub const ERR_CELL_CMD_SMALL_BUFFER: i32 = -6;
/// Error code: a request or response failed to (de)serialize.
pub const ERR_SERIALISATION: i32 = -127;

/// Represents the request to send a command to a cell
#[derive(Serialize, Deserialize, Debug)]
#[allow(missing_docs)]
pub struct CommandRequest {
    pub sri: Sri,
    pub command: Command,
    /// The encoded command payload; `None` for payload-less commands.
    pub payload: Option<Vec<u8>>,
}

/// Represents the request to publish an event
#[derive(Serialize, Deserialize, Debug)]
#[allow(missing_docs)]
pub struct EventPublishRequest {
    pub event: Event,
    /// The encoded event payload; `None` for payload-less events.
    pub payload: Option<Vec<u8>>,
}

/// Represents the request to create a timer (periodic interval or one-shot delay).
#[derive(Serialize, Deserialize, Debug)]
pub struct CreateTimerRequest {
    /// The command export invoked on each tick.
    pub export_name: String,
    /// Delay before the first tick, in milliseconds.
    pub delay_ms: u64,
    /// Tick period in milliseconds; `0` for a one-shot delay.
    pub period_ms: u64,
    /// Maximum number of ticks; `None` runs until the cell stops.
    pub count: Option<u32>,
    /// When `true`, the period is measured from the end of one invocation to
    /// the start of the next (fixed delay) rather than tick-to-tick (fixed
    /// rate).
    pub fixed_delay: bool,
}

/// A request to spawn a new cell instance at runtime.
///
/// Identifies a cell class either by content hash or by registered name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClassRef {
    /// SHA-256 content hash of the Wasm binary.
    Hash([u8; 32]),
    /// Registered class name.
    Name(String),
}

/// Used by cells to create and deploy other cells via the `spawn_cell` host
/// function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnRequest {
    /// The cell class to instantiate.
    pub class: ClassRef,
    /// Local name for the new cell, unique among the caller's children. The
    /// host derives the child's SRI as `child_sri(caller_sri, local_name)` (see
    /// [`crate::cells::naming`]); the caller never supplies a raw SRI.
    pub local_name: Option<String>,
    /// Optional placement tags constraining where the cell is deployed. If
    /// `None`, no placement constraints are applied.
    pub tags: Option<Vec<String>>,
    /// Optional payload delivered to the child's `#[init]` handler as its
    /// argument buffer. `None` (or empty) means the child must have a
    /// no-payload `#[init]`; a non-empty buffer is decoded by the init
    /// handler's `Decoder` param, exactly like a command payload.
    pub arguments: Option<Vec<u8>>,
    /// Decouples the child's lifetime from the caller's: no fencing against
    /// the parent, excluded from cascades, no `cell_lost` on either side.
    /// Declared only by the spawning parent.
    pub detached: bool,
    /// Per-edge fencing tolerance: how long the child outlives this cell's
    /// silence before the runtime kills it. `None` = the cluster default.
    pub grace_ms: Option<u64>,
    /// Per-edge death deadline: how long the child's node must be silent
    /// before it is declared dead (rows released, `cell_lost` sent). Short =
    /// fast failover but misfires on slow reboots; long lets the child ride
    /// one out. Clamped to at least twice the lease renewal period; `None` =
    /// the cluster default.
    pub deadline_ms: Option<u64>,
}

/// Host status code: the call succeeded.
pub const STATUS_OK: core::ffi::c_int = 0;
/// Spawn error code: no cell class with the given hash or name is registered.
pub const SPAWN_ERR_CLASS_NOT_FOUND: core::ffi::c_int = -10;
/// Spawn error code: the caller already has a child with that local name.
pub const SPAWN_ERR_ALREADY_EXISTS: core::ffi::c_int = -11;
/// Spawn error code: the deploy failed after the child's name was claimed.
pub const SPAWN_ERR_DEPLOY_FAILED: core::ffi::c_int = -12;

/// Errors that can occur when spawning a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnError {
    /// No cell class with the given hash or name is registered.
    ClassNotFound,
    /// The caller already has a child with that local name.
    AlreadyExists,
    /// The deploy failed after the child's name was claimed.
    DeployFailed,
}

impl TryFrom<core::ffi::c_int> for SpawnError {
    type Error = core::ffi::c_int;

    fn try_from(code: core::ffi::c_int) -> Result<Self, Self::Error> {
        match code {
            SPAWN_ERR_CLASS_NOT_FOUND => Ok(Self::ClassNotFound),
            SPAWN_ERR_ALREADY_EXISTS => Ok(Self::AlreadyExists),
            SPAWN_ERR_DEPLOY_FAILED => Ok(Self::DeployFailed),
            _ => Err(code),
        }
    }
}

impl From<SpawnError> for &'static str {
    fn from(err: SpawnError) -> Self {
        match err {
            SpawnError::ClassNotFound => "class not found",
            SpawnError::AlreadyExists => "already exists",
            SpawnError::DeployFailed => "deploy failed",
        }
    }
}

/// Reserved system command name carrying a [`CellLost`] payload. Routed to
/// the guest's `on_cell_lost` export, never to a `command_*` handler. The
/// guest-facing send path must reject names with this prefix so cells cannot
/// spoof system notifications.
pub const SYS_CELL_LOST: &str = "__sys_cell_lost";

/// Prefix reserved for host-emitted system commands.
pub const SYS_COMMAND_PREFIX: &str = "__sys";

/// Why a cell died (spec §5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LostReason {
    /// The cell's node lease expired.
    NodeLost,
    /// The cell's task ended without a deliberate undeploy (trap, panic,
    /// runtime error). Its subtree died with it.
    Crashed,
    /// Deliberate `stop_self(code)`; the code is the cell's stated reason.
    Stopped {
        /// The exit code the cell passed to `stop_self`.
        code: Option<u32>,
    },
    /// Killed by an ancestor's terminate or a cascade passing through.
    Terminated,
    /// A spawn this cell requested never came up: the deploy failed after
    /// the child's name was claimed (placement, transfer, or exec failure).
    /// Not sent for detached spawns; the spawn call also returns the error.
    SpawnFailed,
}

/// Notification that a watched cell died. Today only parents receive these
/// (for their children); the shape is deliberately cell-, not child-,
/// centric so general monitoring can reuse it. Delivered as the payload of
/// the reserved [`SYS_CELL_LOST`] command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellLost {
    /// The cell that died (never the receiver of the notification).
    pub cell: Sri,
    /// Spawn-time local name when known; `None` for external deploys.
    /// Helpers need not rely on it — deterministic naming lets a parent
    /// derive its children's SRIs from the names it spawned.
    pub local_name: Option<String>,
    /// Why the cell died.
    pub reason: LostReason,
}

/// Terminate error code: no cell with the given SRI exists.
pub const TERMINATE_ERR_NOT_FOUND: core::ffi::c_int = -20;
/// Terminate error code: the undeploy step failed.
pub const TERMINATE_ERR_UNDEPLOY_FAILED: core::ffi::c_int = -21;
/// Terminate error code: erasing the cell's records failed.
pub const TERMINATE_ERR_ERASE_FAILED: core::ffi::c_int = -22;
/// Terminate error code: the target is not the caller or one of its
/// descendants.
pub const TERMINATE_ERR_NOT_PERMITTED: core::ffi::c_int = -23;

/// Errors that can occur when terminating a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminateError {
    /// No cell with the given SRI exists.
    NotFound,
    /// The undeploy step failed.
    UndeployFailed,
    /// Erasing the cell's records failed.
    EraseFailed,
    /// The target is not the caller or one of its descendants — kill
    /// authority is ancestry (spec §6).
    NotPermitted,
}

impl TryFrom<core::ffi::c_int> for TerminateError {
    type Error = core::ffi::c_int;

    fn try_from(code: core::ffi::c_int) -> Result<Self, Self::Error> {
        match code {
            TERMINATE_ERR_NOT_FOUND => Ok(Self::NotFound),
            TERMINATE_ERR_UNDEPLOY_FAILED => Ok(Self::UndeployFailed),
            TERMINATE_ERR_ERASE_FAILED => Ok(Self::EraseFailed),
            TERMINATE_ERR_NOT_PERMITTED => Ok(Self::NotPermitted),
            _ => Err(code),
        }
    }
}

impl From<TerminateError> for &'static str {
    fn from(err: TerminateError) -> Self {
        match err {
            TerminateError::NotFound => "not found",
            TerminateError::UndeployFailed => "undeploy failed",
            TerminateError::EraseFailed => "erase failed",
            TerminateError::NotPermitted => "not permitted",
        }
    }
}

#[cfg(test)]
mod cell_lost_tests {
    use alloc::string::ToString;

    use super::*;

    #[test]
    fn cell_lost_round_trips_all_reasons() {
        for reason in [
            LostReason::NodeLost,
            LostReason::Crashed,
            LostReason::Stopped { code: Some(3) },
            LostReason::Stopped { code: None },
            LostReason::Terminated,
            LostReason::SpawnFailed,
        ] {
            let v = CellLost {
                cell: sri_of_path("round-trip-test").unwrap().into(),
                local_name: Some("pump".to_string()),
                reason: reason.clone(),
            };
            let bytes = postcard::to_allocvec(&v).unwrap();
            assert_eq!(postcard::from_bytes::<CellLost>(&bytes).unwrap(), v);
        }
    }

    #[test]
    fn sys_cell_lost_is_a_valid_command_name() {
        assert!(Command::new(SYS_CELL_LOST.to_string()).is_ok());
        assert!(SYS_CELL_LOST.starts_with(SYS_COMMAND_PREFIX));
    }
}
