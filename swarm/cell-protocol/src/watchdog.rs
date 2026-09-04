//! Hardware-watchdog reset reporting (SDS-FEAT-2026-HWD-001, Area D).
//!
//! An embedded node that recovers from a hang via its hardware watchdog would
//! otherwise do so silently — a reset loop is indistinguishable from a node
//! flapping in and out of the swarm. After reboot the node writes a
//! [`WatchdogResetReport`] into the watchdog-resets table (keyed by its
//! [`device id`](WatchdogResetReport::device_id)), turning silent recovery
//! into a counted, queryable signal.
//!
//! The row is keyed on the device rather than on the runtime, because a
//! runtime id is regenerated on every boot: keying on it would add a row per
//! reset — growing fastest in exactly the reset loop the table exists to make
//! visible — instead of updating the device's one row.

use serde::{Deserialize, Serialize};

use crate::RuntimeId;
use crate::sys::{string::String, vec::Vec};

/// Which watchdog layer performed the reset.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum WatchdogResetReason {
    /// The staged main watchdog: the stage-0 interrupt recorded the hang,
    /// stage 1 reset the system. The report carries the recorded evidence.
    MwdtStaged,
    /// The RTC-watchdog backstop: the staged path itself failed, so no hang
    /// evidence was recorded — only the reset is known.
    RwdtBackstop,
}

/// Report of hardware-watchdog resets for a device, written by the node after
/// it rebooted from a watchdog reset. Upserted: one report per device, always
/// describing that device's most recent reset.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct WatchdogResetReport {
    /// Stable identity of the device that was reset, and the key its row is
    /// stored under. Survives reboots, so successive reports from one device
    /// land on one row.
    pub device_id: String,
    /// The runtime that wrote the report — the incarnation that came back from
    /// the reset, not the one that hung, since the id is regenerated on every
    /// boot.
    pub runtime_id: RuntimeId,
    /// Watchdog resets since the node's last power-on.
    pub reset_count: u32,
    /// Which watchdog layer performed the most recent reset.
    pub last_reason: WatchdogResetReason,
    /// Node uptime in milliseconds when the hang was detected — present only
    /// when the staged path recorded evidence.
    pub last_uptime_ms: Option<u64>,
    /// Liveness tasks that had stalled when the hang was recorded (empty when
    /// the whole executor was wedged or no evidence was recorded).
    pub stale_tasks: Vec<String>,
}
