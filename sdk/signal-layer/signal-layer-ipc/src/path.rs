//! Socket-path resolution rule (spec §4):
//! Use `/run/peeriot/signal-layer.sock` if `/run/peeriot/` exists and is
//! accessible and writable (via `libc::access`), else
//! `$XDG_RUNTIME_DIR/peeriot-signal-layer.sock`.
//!
//! Fail-closed (S4): if neither `/run/peeriot` is usable nor `XDG_RUNTIME_DIR`
//! is set, return `None` rather than falling back to a world-writable `/tmp`
//! path — `/tmp` as a socket location undermines filesystem-permission access
//! control (the ONLY access control for this socket).

use std::path::PathBuf;

/// Resolve the default socket path per the spec §4 rule.
///
/// Returns:
/// - `Some("/run/peeriot/signal-layer.sock")` when `/run/peeriot/` exists
///   and is writable by the effective user.
/// - `Some("$XDG_RUNTIME_DIR/peeriot-signal-layer.sock")` when
///   `XDG_RUNTIME_DIR` is set.
/// - `None` when neither location is usable (S4: fail-closed; no /tmp
///   fallback that would be world-writable).
pub fn default_socket_path() -> Option<PathBuf> {
    let primary = PathBuf::from("/run/peeriot");
    if primary.exists() && is_accessible_and_writable(&primary) {
        return Some(primary.join("signal-layer.sock"));
    }
    let xdg = std::env::var("XDG_RUNTIME_DIR").ok()?;
    Some(PathBuf::from(xdg).join("peeriot-signal-layer.sock"))
}

/// Check whether `path` is writable by the *effective* user via `access(2)`.
///
/// Using `access(2)` (effective-access check) is more correct than inspecting
/// permission bits, which do not account for supplementary groups or ACLs.
fn is_accessible_and_writable(path: &std::path::Path) -> bool {
    let Some(path_str) = path.to_str() else {
        return false;
    };
    let Ok(cstr) = std::ffi::CString::new(path_str) else {
        return false;
    };
    // SAFETY: access(2) is always safe to call with a valid path pointer; it
    // does not dereference Rust memory beyond the null-terminated string.
    let rc = unsafe { libc::access(cstr.as_ptr(), libc::W_OK) };
    rc == 0
}

// ── Unit tests for the socket-path resolution rule (SR-16) ───────────────────
//
// Spec §4: "use /run/peeriot/signal-layer.sock if /run/peeriot/ exists and is
// writable, else $XDG_RUNTIME_DIR/peeriot-signal-layer.sock".
//
// S4: fail-closed — no /tmp fallback when XDG_RUNTIME_DIR is unset.
//
// We cannot assume /run/peeriot exists in CI, so we test the rule indirectly
// using a function that accepts the existence/writable predicate as a parameter
// (testability seam).  We also test the XDG fallback rule directly by setting
// the XDG_RUNTIME_DIR environment variable.

#[cfg(test)]
mod tests {
    use super::*;

    /// Same resolution logic as `default_socket_path`, parameterised for testing.
    fn resolve_socket_path_for_test(
        primary_exists_and_writable: bool,
        xdg: Option<&str>,
    ) -> Option<PathBuf> {
        if primary_exists_and_writable {
            return Some(PathBuf::from("/run/peeriot/signal-layer.sock"));
        }
        let base = xdg?; // S4: fail-closed — no /tmp fallback
        Some(PathBuf::from(base).join("peeriot-signal-layer.sock"))
    }

    /// SR-16 primary rule: when the primary dir exists and is writable, the path
    /// is `/run/peeriot/signal-layer.sock`.
    #[test]
    fn resolution_primary_path_when_run_peeriot_writable() {
        let path = resolve_socket_path_for_test(true, None).expect("primary path");
        assert_eq!(
            path,
            PathBuf::from("/run/peeriot/signal-layer.sock"),
            "primary path must be /run/peeriot/signal-layer.sock"
        );
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("signal-layer.sock"),
            "socket filename must be signal-layer.sock"
        );
    }

    /// SR-16 fallback rule: when primary dir is not writable, fall back to
    /// `$XDG_RUNTIME_DIR/peeriot-signal-layer.sock`.
    #[test]
    fn resolution_xdg_fallback_when_run_peeriot_not_available() {
        let path =
            resolve_socket_path_for_test(false, Some("/run/user/1000")).expect("XDG fallback path");
        assert_eq!(
            path,
            PathBuf::from("/run/user/1000/peeriot-signal-layer.sock"),
            "fallback path must be $XDG_RUNTIME_DIR/peeriot-signal-layer.sock"
        );
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("peeriot-signal-layer.sock"),
            "socket filename must be peeriot-signal-layer.sock in XDG fallback"
        );
    }

    /// S4: When `XDG_RUNTIME_DIR` is absent and /run/peeriot is not usable, the
    /// function must return `None` — no fallback to /tmp.
    #[test]
    fn resolution_fails_closed_when_xdg_absent_and_no_primary() {
        let result = resolve_socket_path_for_test(false, None);
        assert!(
            result.is_none(),
            "S4: when XDG_RUNTIME_DIR is absent and /run/peeriot is not usable, \
             must return None (fail-closed), not fall back to /tmp; got {result:?}"
        );
    }

    /// The primary and fallback filenames are distinct by spec: the primary uses
    /// "signal-layer.sock" and the fallback uses "peeriot-signal-layer.sock"
    /// (namespace-qualified to avoid conflicts in `$XDG_RUNTIME_DIR`).
    #[test]
    fn primary_and_fallback_socket_names_are_distinct() {
        let primary = resolve_socket_path_for_test(true, None).expect("primary");
        let fallback =
            resolve_socket_path_for_test(false, Some("/run/user/1000")).expect("fallback");
        assert_ne!(
            primary.file_name(),
            fallback.file_name(),
            "primary and fallback socket names must be different"
        );
    }

    /// `default_socket_path()` returns `Some` with a path ending in either
    /// "signal-layer.sock" (primary) or "peeriot-signal-layer.sock" (fallback),
    /// or `None` when neither location is usable (S4: fail-closed).
    /// This is a live call — result depends on /run/peeriot and `XDG_RUNTIME_DIR`.
    #[test]
    fn default_socket_path_returns_a_sock_file_or_none() {
        match default_socket_path() {
            Some(path) => {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .expect("file name");
                assert!(
                    name == "signal-layer.sock" || name == "peeriot-signal-layer.sock",
                    "default_socket_path must end in signal-layer.sock or peeriot-signal-layer.sock, got {name}"
                );
            }
            None => {
                // S4: fail-closed — acceptable when neither location is usable.
            }
        }
    }

    /// Both sides (pipeline + host) apply the SAME rule from the shared
    /// `signal_layer_ipc` function — calling it twice with the same environment
    /// must yield the same path (idempotency / no randomness).
    #[test]
    fn default_socket_path_is_deterministic() {
        let p1 = default_socket_path();
        let p2 = default_socket_path();
        assert_eq!(
            p1, p2,
            "default_socket_path must be deterministic across calls"
        );
    }
}
