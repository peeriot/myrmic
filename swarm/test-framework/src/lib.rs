#![allow(clippy::map_unwrap_or)]
#![allow(clippy::unwrap_used)]

//! Helpers and abstractions for end-to-end tests of myrmic: starting real runtimes, alone or as a
//! swarm, operating them the way a user does (through the `myrmic` CLI), and injecting the faults
//! a deployed swarm meets (crashes, freezes, power loss, network splits, full disks, clock steps).
//!
//! # Where to start
//!
//! - **One runtime:** [`myrmic`]. A [`myrmic::Myrmic`] is the CLI on some machine; it deploys
//!   cells, sends commands and reads status, and [`myrmic::Myrmic::runtime`] describes a runtime
//!   it can start. It comes from its machine, a [`host`] (the test's own, a remote one over SSH,
//!   a container).
//! - **Who links to whom:** [`overlay`], [`World`] and [`host_set`]. Every runtime is a member of an
//!   [`overlay::Overlay`], attached on one of its multicast groups or by a connect to another
//!   member; the overlay sits on a world, whose host sets decide whether one host can dial
//!   another at all.
//! - **A runtime's lifecycle:** [`runtime`]. Start, stop, kill and pause, with the stopped and
//!   the running state as separate types, so a call in the wrong state does not compile. The
//!   same handles serve single-runtime tests and swarms.
//! - **A node inside the test process:** [`in_process`], for what a CLI runtime cannot give: a
//!   node with only some plugins, a zid the test chooses, its log events in the test process.
//! - **Several runtimes:** [`swarm`]. A [`swarm::Swarm`] holds the runtimes a test added to it,
//!   of any kind, and each start of one waits until they are linked as their overlay says.
//! - **Talking to a swarm's services directly,** for what the CLI does not cover: its db
//!   ([`swarm::Swarm::get_db`]) and its sorg client ([`swarm::Swarm::get_sorg`]).
//! - **Waiting for something to become true:** [`wait`] ([`wait_for`] when the test needs a
//!   value from the state it waits for, [`assert_eventually!`], [`wait_until`]).
//! - **Finding the binaries under test:** [`resolve_binary!`] and [`binary`].
//!
//! Infrastructure, rarely used directly: [`docker`] (containers and networks, behind
//! [`myrmic::Docker`]).

pub mod binary;
pub mod docker;
pub mod host;
pub mod in_process;
pub mod myrmic;
pub mod overlay;
pub mod runtime;
pub mod swarm;
pub mod wait;
mod world;

pub use wait::{wait_for, wait_until};
pub use world::*;

/// Run the cleanup `command` of the drop-guard `guard`. A failure is reported, not raised: a panic
/// during unwinding aborts the process and hides the original one.
fn run_cleanup(guard: &str, mut command: std::process::Command) {
    let invocation = format!("{command:?}");
    match command.output() {
        Ok(output) if output.status.success() => {}
        Ok(output) => eprintln!(
            "{guard} drop-guard: {invocation} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(err) => eprintln!("{guard} drop-guard: failed to run {invocation}: {err}"),
    }
}

/// for [`start_runtimes!`], which expands to `futures::join!` in the caller's crate
#[doc(hidden)]
pub use futures as __futures;

/// Resolve a binary by name, built for the host or for a target triple, in the target directory
/// of the workspace test-framework is built in (see [`binary::resolve`]).
///
/// Resolution order:
/// 1. the `{NAME_UPPER}_BINARY` env var (e.g. `SWARM_BINARY`, `MYRMIC_BINARY`), or
///    `{NAME_UPPER}_{TRIPLE_UPPER}_BINARY` for a triple
/// 2. `<workspace>/target/<profile>/<name>` for a pinned profile, otherwise the most recently
///    built `<name>` of any profile (`<workspace>/target/<triple>/…` for a triple)
///
/// `triple` and `profile` are optional, in that order:
/// - `resolve_binary!("myrmic")`: the host's newest build
/// - `resolve_binary!("myrmic", profile = "release")`: the host's release build
/// - `resolve_binary!("myrmic", triple = "aarch64-unknown-linux-gnu", profile = "dist")`
#[macro_export]
macro_rules! resolve_binary {
    ($name:expr $(, triple = $triple:expr)? $(, profile = $profile:expr)?) => {
        $crate::binary::resolve(
            $name,
            None::<&str> $(.or(Some($triple)))?,
            None::<&str> $(.or(Some($profile)))?,
        )
    };
}

/// Path to a file under the calling crate's `assets/` directory.
///
/// `asset!("swarm_configs/foo.jsonnet")` expands to
/// `<caller CARGO_MANIFEST_DIR>/assets/swarm_configs/foo.jsonnet`.
#[macro_export]
macro_rules! asset {
    ($path:literal) => {
        concat!(env!("CARGO_MANIFEST_DIR"), "/assets/", $path)
    };
}

/// Captured output of a finished command (on the host, over SSH or inside a container).
#[derive(Debug, Clone)]
pub struct CommandOutput {
    /// whether the command exited with code 0
    pub success: bool,
    /// captured stdout, lossily decoded as UTF-8
    pub stdout: String,
    /// captured stderr, lossily decoded as UTF-8
    pub stderr: String,
}

impl From<std::process::Output> for CommandOutput {
    fn from(output: std::process::Output) -> Self {
        Self {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn asset_macro_builds_caller_relative_path() {
        let path = crate::asset!("configs/foo.jsonnet");
        assert!(path.ends_with("/assets/configs/foo.jsonnet"));
        assert!(path.starts_with(env!("CARGO_MANIFEST_DIR")));
    }
}
