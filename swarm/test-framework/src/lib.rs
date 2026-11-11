#![allow(clippy::map_unwrap_or)]
#![allow(clippy::unwrap_used)]

//! Helpers and abstractions for end-to-end tests of myrmic: starting real runtimes, alone or as a
//! swarm, operating them the way a user does (through the `myrmic` CLI), and injecting the faults
//! a deployed swarm meets (network splits, full disks, clock steps, crashes, power loss).
//!
//! # Where to start
//!
//! - **One runtime:** [`myrmic`]. A [`myrmic::Myrmic`] is the CLI on some machine (the host, a
//!   container, a remote host over SSH); it deploys cells, sends commands and reads status, and
//!   [`myrmic::Myrmic::runtime`] describes a runtime it can start.
//! - **A runtime's lifecycle:** [`runtime`]. Start, stop, kill and pause, with the stopped and
//!   the running state as separate types, so a call in the wrong state does not compile. The
//!   same handles serve single-runtime tests and swarms.
//! - **Several runtimes:** [`swarm2`]. A [`swarm2::Swarm`] starts the runtimes a
//!   [`swarm2::SwarmSpec`] names on a backend (the host, docker containers, remote hosts over
//!   SSH), waits until they are connected, and offers the faults its backend can produce as
//!   capability traits ([`swarm2::backend`]).
//! - **Talking to a swarm's services directly:** [`clients`] (db, sorg), for what the CLI does
//!   not cover.
//! - **Waiting for something to become true:** [`wait`] ([`assert_eventually!`],
//!   [`wait_until`]).
//! - **Finding the binaries under test:** [`resolve_binary!`] and [`binary`].
//!
//! Infrastructure the above builds on, rarely used directly: [`docker`] (containers and
//! networks), [`ssh_utils`] (remote hosts), [`shelly`] (smart plugs that cut a host's power).
//!
//! # Older modules
//!
//! [`swarm`], [`scenario`], [`cell`], [`compose`], [`sidecar`] and [`rack`] are the previous way
//! of spawning swarms; [`swarm2`] replaces them and takes over the name `swarm` once they are
//! gone. [`latency`], [`metrics`], [`producers`] and [`telemetry_files`] serve the benchmarks and
//! move into a crate of their own. New tests use neither.

pub mod binary;
pub mod cell;
pub mod clients;
pub mod compose;
pub mod docker;
pub mod latency;
pub mod metrics;
pub mod myrmic;
pub mod producers;
pub mod rack;
pub mod runtime;
pub mod scenario;
pub mod shelly;
pub mod sidecar;
pub mod ssh_utils;
pub mod swarm;
pub mod swarm2;
pub mod telemetry_files;
pub mod wait;

use cell_protocol::Sri;
use swarm_telemetry::db::opentelemetry_proto::tonic::{
    common::v1::any_value::Value, trace::v1::Span,
};
pub use wait::wait_until;

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

pub trait SriAttribute {
    fn sri(&self) -> Option<Sri>;
}

impl SriAttribute for Span {
    fn sri(&self) -> Option<Sri> {
        self.attributes.iter().find_map(|kv| {
            let key = kv.key.as_str();
            match key {
                // TODO: on another feature branch we rename `module_id` to `sri`; for now support both,
                // but remove `module_id` later
                "sri" | "module_id" => {
                    let sri_value = kv.value.as_ref()?.value.as_ref()?;
                    if let Value::StringValue(sri) = sri_value {
                        sri.parse().ok()
                    } else {
                        None
                    }
                }
                _ => None,
            }
        })
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
