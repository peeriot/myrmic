#![allow(clippy::map_unwrap_or)]
#![allow(clippy::unwrap_used)]

//! The test-framework provides helpers and abstractions to easily write end to end tests of three
//! categories:
//! - e2e testing CLI tools
//! - running tests against a running swarm
//! - running tests with network shaping

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
pub mod scenario;
pub mod sidecar;
pub mod swarm;
pub mod swarm2;
pub mod telemetry_files;
pub mod wait;

use cell_protocol::Sri;
use swarm_telemetry::db::opentelemetry_proto::tonic::{
    common::v1::any_value::Value, trace::v1::Span,
};
pub use wait::wait_until;

/// Resolve a binary by name using the workspace target directory of the calling
/// crate, with optional override via `{NAME_UPPER}_BINARY` env var.
///
/// Resolution order:
/// 1. `{NAME_UPPER}_BINARY` env var (e.g. `SWARM_BINARY`, `MYRMIC_BINARY`)
/// 2. `<workspace>/target/release/<name>` if it exists
/// 3. `<workspace>/target/debug/<name>` if it exists
#[macro_export]
macro_rules! resolve_binary {
    ($name:literal) => {
        $crate::binary::resolve(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target"), $name)
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

/// Optional SSH identity file (`ssh -i`/`scp -i`) applied to every SSH/SCP connection the
/// test-framework opens (the myrmic-over-SSH backend, rack provisioning) — set `SSH_IDENTITY_FILE`
/// when the target hosts require a non-default key. Read fresh on every call (not cached) so
/// tests that set the env var mid-run see the change take effect.
#[must_use]
pub fn ssh_identity_file() -> Option<String> {
    std::env::var("SSH_IDENTITY_FILE")
        .ok()
        .filter(|value| !value.is_empty())
}

/// Optional `known_hosts` file (`ssh -o UserKnownHostsFile=`/`scp -o UserKnownHostsFile=`)
/// applied to every SSH/SCP connection the test-framework opens — set `SSH_KNOWN_HOSTS_FILE` to
/// pin the exact file host-key trust was written to (e.g. by a preceding `ssh-keyscan` step),
/// rather than relying on `ssh`'s own default (`~/.ssh/known_hosts`, resolved from `$HOME` and
/// whatever `ssh_config` the environment ships), which isn't guaranteed to be the file a CI setup
/// step actually populated. Read fresh on every call (not cached), matching
/// [`ssh_identity_file`].
#[must_use]
pub fn ssh_known_hosts_file() -> Option<String> {
    std::env::var("SSH_KNOWN_HOSTS_FILE")
        .ok()
        .filter(|value| !value.is_empty())
}

/// `ssh` with the options every SSH connection the test-framework opens shares:
/// [`ssh_identity_file`] and [`ssh_known_hosts_file`], where set.
fn ssh_command() -> std::process::Command {
    let mut command = std::process::Command::new("ssh");
    if let Some(identity_file) = ssh_identity_file() {
        command.arg("-i").arg(identity_file);
    }
    if let Some(known_hosts_file) = ssh_known_hosts_file() {
        command
            .arg("-o")
            .arg(format!("UserKnownHostsFile={known_hosts_file}"));
    }
    command
}

/// The remote invocation of `program args` as a single, fully quoted shell
/// word sequence.
///
/// `ssh` joins the command arguments it is handed with spaces and gives the
/// result to a shell on the far side, which splits and expands it all over
/// again — so passing them as separate argv entries quotes nothing. An app
/// spec under a path with a space arrives as two arguments, and a `;`,
/// backtick or `$(…)` in a tag or a command payload runs as a command on
/// the rack node. [`myrmic::Local`] is unaffected because it
/// execs directly; SSH is the only path with a shell in it.
fn remote_command(program: &str, args: &[&str]) -> String {
    std::iter::once(program)
        .chain(args.iter().copied())
        .map(shell_quote)
        .collect::<Vec<_>>()
        .join(" ")
}

/// One argument as a single shell word the remote shell cannot split or expand.
///
/// Single quotes suppress every expansion; the one character they cannot carry
/// is a single quote itself, which is closed, backslash-escaped and reopened.
fn shell_quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', r"'\''"))
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
    use super::remote_command;

    #[test]
    fn asset_macro_builds_caller_relative_path() {
        let path = crate::asset!("configs/foo.jsonnet");
        assert!(path.ends_with("/assets/configs/foo.jsonnet"));
        assert!(path.starts_with(env!("CARGO_MANIFEST_DIR")));
    }

    #[test]
    fn a_path_with_a_space_stays_one_argument() {
        assert_eq!(
            remote_command("myrmic", &["deploy", "/home/user/my benchmarks/app.yml"]),
            "'myrmic' 'deploy' '/home/user/my benchmarks/app.yml'",
        );
    }

    #[test]
    fn shell_metacharacters_do_not_reach_the_remote_shell() {
        // Unquoted, the remote shell would run `id` and `rm -rf /` as commands
        // of their own on the rack node.
        assert_eq!(
            remote_command("myrmic", &["send", "cell/one", "go; rm -rf / $(id)"]),
            "'myrmic' 'send' 'cell/one' 'go; rm -rf / $(id)'",
        );
    }

    #[test]
    fn a_single_quote_is_closed_escaped_and_reopened() {
        assert_eq!(
            remote_command("myrmic", &["send", "it's"]),
            r"'myrmic' 'send' 'it'\''s'",
        );
    }
}
