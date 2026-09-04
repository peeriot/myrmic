//! Per-root restart policy: the declarative supervision directive an app root
//! carries so the orchestrator can restart it after a failure. Enforced
//! host-side only (roots are Linux-deployed); the guest never sees it.

use myrmic_common::cells::LostReason;
use serde::{Deserialize, Serialize};

/// When a root should be restarted after it dies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RestartType {
    /// Never restart (current behavior); the root's death is terminal.
    #[default]
    Never,
    /// Restart only on an abnormal exit (crash, spawn failure, node loss, or a
    /// non-zero stop code). A clean `stop_self(0)` is terminal.
    OnError,
    /// Restart on any exit, including a clean self-stop. Only an operator
    /// terminate / cascade (`Terminated`) is terminal.
    Always,
}

/// A root's full restart policy: the trigger type plus the crash-loop bounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestartPolicy {
    pub restart_type: RestartType,
    /// Maximum restarts permitted within `window_ms` before giving up.
    pub max_restarts: u32,
    /// Sliding window (ms) over which `max_restarts` is counted.
    pub window_ms: u64,
    /// Fixed delay (ms) between a death and its restart attempt.
    pub delay_ms: u64,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            restart_type: RestartType::Never,
            max_restarts: 5,
            window_ms: 60_000,
            delay_ms: 1_000,
        }
    }
}

impl RestartPolicy {
    /// A policy that actually restarts (anything other than `Never`).
    pub fn is_enabled(&self) -> bool {
        self.restart_type != RestartType::Never
    }
}

/// Whether a death with `reason` warrants a restart under `restart_type`.
pub fn should_restart(restart_type: RestartType, reason: &LostReason) -> bool {
    match restart_type {
        RestartType::Never => false,
        RestartType::OnError => is_abnormal(reason),
        // Any exit restarts, except an operator terminate / cascade kill.
        RestartType::Always => !matches!(reason, LostReason::Terminated),
    }
}

/// An abnormal exit: a crash, a failed spawn, a lost node, or a non-zero stop
/// code. A clean `stop_self(0)` and an operator terminate are *not* abnormal.
fn is_abnormal(reason: &LostReason) -> bool {
    match reason {
        LostReason::Crashed | LostReason::NodeLost | LostReason::SpawnFailed => true,
        LostReason::Stopped { code } => matches!(code, Some(c) if *c != 0),
        LostReason::Terminated => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_is_never_with_bounded_defaults() {
        let p = RestartPolicy::default();
        assert_eq!(p.restart_type, RestartType::Never);
        assert_eq!(p.max_restarts, 5);
        assert_eq!(p.window_ms, 60_000);
        assert_eq!(p.delay_ms, 1_000);
    }

    #[test]
    fn is_enabled_is_false_only_for_never() {
        assert!(!RestartPolicy::default().is_enabled());
        assert!(
            RestartPolicy {
                restart_type: RestartType::OnError,
                ..Default::default()
            }
            .is_enabled()
        );
        assert!(
            RestartPolicy {
                restart_type: RestartType::Always,
                ..Default::default()
            }
            .is_enabled()
        );
    }

    #[test]
    fn never_restarts_on_nothing() {
        for reason in [
            LostReason::Crashed,
            LostReason::NodeLost,
            LostReason::SpawnFailed,
            LostReason::Terminated,
            LostReason::Stopped { code: Some(1) },
            LostReason::Stopped { code: Some(0) },
            LostReason::Stopped { code: None },
        ] {
            assert!(!should_restart(RestartType::Never, &reason), "{reason:?}");
        }
    }

    #[test]
    fn on_error_restarts_only_on_abnormal_exit() {
        assert!(should_restart(RestartType::OnError, &LostReason::Crashed));
        assert!(should_restart(RestartType::OnError, &LostReason::NodeLost));
        assert!(should_restart(
            RestartType::OnError,
            &LostReason::SpawnFailed
        ));
        assert!(should_restart(
            RestartType::OnError,
            &LostReason::Stopped { code: Some(1) }
        ));

        assert!(!should_restart(
            RestartType::OnError,
            &LostReason::Terminated
        ));
        assert!(!should_restart(
            RestartType::OnError,
            &LostReason::Stopped { code: Some(0) }
        ));
        assert!(!should_restart(
            RestartType::OnError,
            &LostReason::Stopped { code: None }
        ));
    }

    #[test]
    fn always_restarts_on_every_exit_except_terminate() {
        assert!(should_restart(RestartType::Always, &LostReason::Crashed));
        assert!(should_restart(RestartType::Always, &LostReason::NodeLost));
        assert!(should_restart(
            RestartType::Always,
            &LostReason::SpawnFailed
        ));
        assert!(should_restart(
            RestartType::Always,
            &LostReason::Stopped { code: Some(1) }
        ));
        assert!(should_restart(
            RestartType::Always,
            &LostReason::Stopped { code: Some(0) }
        ));
        assert!(should_restart(
            RestartType::Always,
            &LostReason::Stopped { code: None }
        ));

        assert!(!should_restart(
            RestartType::Always,
            &LostReason::Terminated
        ));
    }
}
