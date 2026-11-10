//! Where the myrmic CLI runs: on the host, on a remote host over SSH, or inside a docker
//! container.

use std::sync::atomic::{AtomicU16, Ordering};

mod docker;
mod local;
mod ssh;

pub use docker::Docker;
pub use local::Local;
pub use ssh::Ssh;

/// Where and how the myrmic CLI is executed.
///
/// A backend only builds host commands. [`super::Myrmic`] runs them and holds all knowledge of
/// the CLI's commands and output, so the backends cannot drift apart.
pub trait MyrmicBackend: Clone {
    /// `program args` as a command on the host that runs them on the target: the program itself,
    /// or the `ssh`/`docker exec` invocation around it
    fn command(&self, program: &str, args: &[&str]) -> std::process::Command;

    /// Exit codes that belong to the wrapper around the program (`ssh` could not connect,
    /// `docker exec` could not reach the daemon, the program is missing on the target) rather
    /// than to the program: broken test infrastructure, not a result.
    fn wrapper_exit_codes(&self) -> &'static [i32];

    /// The runtime config (YAML) every runtime started here needs, e.g. for how it finds the
    /// others in this environment; `None` when the default config does. A runtime started here
    /// takes no other config.
    fn runtime_config(&self) -> Option<String>;
}

/// A multicast group no other isolation uses: the address from this process's pid
/// (administratively scoped 239.0.0.0/8; its low 24 bits cover every Linux pid), the port from a
/// per-process counter.
pub(crate) fn isolated_multicast_group() -> String {
    static NEXT_PORT: AtomicU16 = AtomicU16::new(7446);
    let port = NEXT_PORT.fetch_add(1, Ordering::Relaxed);
    assert!(
        port >= 7446,
        "ran out of ports for isolated multicast groups"
    );
    let pid = std::process::id();
    format!(
        "239.{}.{}.{}:{port}",
        (pid >> 16) & 0xFF,
        (pid >> 8) & 0xFF,
        pid & 0xFF
    )
}

/// the runtime config that has a runtime scout on `multicast_group`
fn isolated_runtime_config(multicast_group: &str) -> String {
    // JSON is YAML, which `runtimes start` parses
    serde_json::json!({
        "zenoh": {
            "scouting": { "multicast": { "address": multicast_group } },
        },
    })
    .to_string()
}
