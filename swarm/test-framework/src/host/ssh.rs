//! Machines the test reaches over SSH. [`Ssh`] (also exported as [`super::Ssh`]) is such a machine,
//! for running runtimes and routers on it. [`wait_until_reachable`] and
//! [`wait_until_clock_synchronized`] wait until a machine is back after a reboot; they only need
//! the machine's SSH destination.

use std::{
    net::IpAddr,
    path::Path,
    process::{Output, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};

use super::{Id, Remote, Router, Shared};
use crate::{
    myrmic::{self, Myrmic},
    overlay,
};

/// How long one attempt of a wait for a machine may take to connect: a machine that is still
/// booting doesn't answer at all.
const CONNECT_TIMEOUT_SECS: &str = "5";

/// How long a wait for a machine pauses between two attempts.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// A machine the test reaches over SSH, e.g. a Raspberry Pi of the rack. It exists before it is in
/// any host set; a host set takes it in and gives it its address there. Clones are the same host.
///
/// Every SSH connection to the machine takes these options from the environment, where they are
/// set:
///
/// - `SSH_IDENTITY_FILE`: the identity file;
/// - `SSH_KNOWN_HOSTS_FILE`: the known hosts file, e.g. one that a CI setup prepared, instead of
///   `ssh`'s default;
/// - `SSH_STRICT_HOST_KEY_CHECKING`: the host key policy, e.g. `accept-new` in a CI job whose fresh
///   container knows no hosts yet.
#[derive(Clone)]
pub struct Ssh {
    /// the SSH destination, e.g. `user@rack-node-1.example`: anything `ssh` accepts, including host
    /// aliases from `~/.ssh/config`. It also names the host in messages.
    destination: Arc<str>,
    /// the directory the machine's `myrmic` and `zenoh-router` lie in (the test puts them there,
    /// see [`Self::upload`])
    bin_dir: Arc<str>,
    shared: Arc<Shared>,
}

impl Ssh {
    /// The machine at the SSH destination `destination`, with its `myrmic` and `zenoh-router`
    /// binaries in `bin_dir`. It starts with nothing in its firewall and in no host set.
    pub fn new(destination: impl Into<String>, bin_dir: impl Into<String>) -> Self {
        Self {
            destination: Arc::from(destination.into()),
            bin_dir: Arc::from(bin_dir.into()),
            shared: Arc::default(),
        }
    }

    /// its identity
    pub(crate) fn get_id(&self) -> Id {
        Id {
            shared: Arc::downgrade(&self.shared),
            label: self.destination.clone(),
        }
    }

    /// record its address on the host set named `host_set`, which takes it in
    pub(crate) fn set_address(&self, host_set: &str, address: IpAddr) {
        self.shared
            .lock_addresses()
            .insert(host_set.to_owned(), address);
    }

    /// its SSH destination
    pub fn get_destination(&self) -> &str {
        &self.destination
    }

    /// The CLI on this machine, attached by `attachment`. Like [`super::Local::myrmic_isolated`],
    /// it keeps its state in directories of its own. They are created on the machine here and
    /// removed there with the last clone of the shim, so its runtimes see no name, identity or data
    /// of an earlier run.
    pub fn myrmic_isolated(
        &self,
        attachment: impl Into<overlay::Attachment>,
    ) -> Myrmic<myrmic::Ssh> {
        Myrmic::new(
            myrmic::Ssh::isolated(self.clone()),
            format!("{}/myrmic", self.bin_dir),
            attachment.into(),
        )
    }

    /// A zenoh router on this machine, as a new member of an overlay, attached by `attachment`.
    /// Returns once the router listens, so a swarm can attach through it right away (see
    /// [`Router`]).
    pub async fn router(&self, attachment: impl Into<overlay::Attachment>) -> Router<Self> {
        let member = attachment.into().add_member(self.get_id());
        let binary = format!("{}/zenoh-router", self.bin_dir);
        Router::start(self.clone(), &binary, member).await
    }

    /// Copy the local binary `local` into the machine's binary directory (see [`Self::new`]), under
    /// its own file name. Returns once the file is on the machine's disk, so a test that cuts the
    /// machine's power doesn't lose it. If the machine already has a file with the same content
    /// there, nothing is copied. Panics if the copy fails.
    ///
    /// To upload to several machines, run the uploads at the same time, e.g. with
    /// `futures::future::join_all`.
    pub async fn upload(&self, local: &Path) {
        let hash = sha256_of(local);
        let name = local
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_else(|| panic!("{} has no UTF-8 file name", local.display()));
        let remote_path = format!("{}/{name}", self.bin_dir);

        // `mkdir` first, so a missing directory doesn't fail the `scp` below
        let check = self
            .run(self.ssh(
                &["-o", "BatchMode=yes"],
                &[
                    "sh",
                    "-c",
                    r#"mkdir -p "$(dirname "$1")" && sha256sum "$1""#,
                    "sh",
                    &remote_path,
                ],
            ))
            .await;
        // 255 is `ssh` itself failing; any other failure means there is no file to compare (the
        // first upload) or the `mkdir` failed, which the `scp` then reports
        assert_ne!(
            check.status.code(),
            Some(255),
            "ssh to {} failed: {}",
            self.destination,
            String::from_utf8_lossy(&check.stderr)
        );
        if !check.status.success() || !String::from_utf8_lossy(&check.stdout).starts_with(&hash) {
            let mut scp = command_with_ssh_config("scp");
            scp.args(["-q", "-p", "-o", "BatchMode=yes"])
                .arg(local)
                .arg(format!("{}:{remote_path}", self.destination));
            let copy = self.run(scp).await;
            assert!(
                copy.status.success(),
                "uploading {} to {}:{remote_path} failed: {}",
                local.display(),
                self.destination,
                String::from_utf8_lossy(&copy.stderr)
            );

            // the file and, for a new one, its name in the directory
            let sync = self
                .run(self.ssh(
                    &["-o", "BatchMode=yes"],
                    &[
                        "sh",
                        "-c",
                        r#"sync -- "$1" "$(dirname "$1")""#,
                        "sh",
                        &remote_path,
                    ],
                ))
                .await;
            assert!(
                sync.status.success(),
                "syncing {}:{remote_path} failed: {}",
                self.destination,
                String::from_utf8_lossy(&sync.stderr)
            );
        }
    }

    /// `ssh <options> <destination> <words>`, the words quoted so the remote shell runs them as
    /// they are
    pub(crate) fn ssh(&self, options: &[&str], words: &[&str]) -> std::process::Command {
        ssh_command(&self.destination, options, words)
    }

    /// the output of `command`, a command for this machine, once it has finished
    async fn run(&self, command: std::process::Command) -> Output {
        tokio::process::Command::from(command)
            .stdin(Stdio::null())
            .output()
            .await
            .unwrap_or_else(|err| panic!("failed to run a command for {}: {err}", self.destination))
    }
}

impl Remote for Ssh {
    /// `ssh [-i identity] [-o UserKnownHostsFile=..] [-o StrictHostKeyChecking=..] <destination>
    /// <quoted program args>`
    fn command(&self, program: &str, args: &[&str]) -> std::process::Command {
        let mut words = vec![program];
        words.extend(args);
        self.ssh(&[], &words)
    }
}

/// Wait until the machine at the SSH destination `destination` accepts SSH connections, e.g. after
/// a reboot. Panics after `timeout`. Connects with the same options from the environment as an
/// [`Ssh`].
pub fn wait_until_reachable(destination: &str, timeout: Duration) {
    poll(
        destination,
        &["true"],
        timeout,
        "to take SSH connections",
        |output| output.status.success(),
    );
}

/// Wait until the machine at the SSH destination `destination` has synchronized its wall clock
/// (systemd's `NTPSynchronized`), e.g. after it booted without a clock that kept the time. Panics
/// after `timeout`. Connects with the same options from the environment as an [`Ssh`].
pub fn wait_until_clock_synchronized(destination: &str, timeout: Duration) {
    let show = [
        "timedatectl",
        "show",
        "--property",
        "NTPSynchronized",
        "--value",
    ];
    poll(
        destination,
        &show,
        timeout,
        "to synchronize its clock",
        |output| output.status.success() && output.stdout.trim_ascii() == b"yes",
    );
}

/// Run `words` on the machine at `destination` again and again until `done` accepts the output.
/// Panics after `timeout`; the message says what the machine failed `to` do and shows the last
/// output.
fn poll(
    destination: &str,
    words: &[&str],
    timeout: Duration,
    to: &str,
    done: impl Fn(&Output) -> bool,
) {
    let connect_timeout = format!("ConnectTimeout={CONNECT_TIMEOUT_SECS}");
    let run = || {
        ssh_command(
            destination,
            &["-o", "BatchMode=yes", "-o", &connect_timeout],
            words,
        )
        .output()
        .unwrap_or_else(|err| panic!("failed to run ssh: {err}"))
    };
    let start = Instant::now();
    let mut output = run();
    while !done(&output) {
        assert!(
            start.elapsed() < timeout,
            "{destination} failed {to} within {timeout:?}; last: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        std::thread::sleep(POLL_INTERVAL);
        output = run();
    }
}

/// `ssh <options> <destination> <words>`, with the options from the environment (see [`Ssh`]) and
/// the words quoted so the remote shell runs them as they are
fn ssh_command(destination: &str, options: &[&str], words: &[&str]) -> std::process::Command {
    let mut command = command_with_ssh_config("ssh");
    command
        .args(options)
        .arg(destination)
        .arg(quoted_command_line(words));
    command
}

/// `program` (`ssh` or `scp`, which take the same options) with the options from the environment
/// that every SSH connection uses (see [`Ssh`]). The options are read on every call, so a test that
/// sets one during its run sees the change.
fn command_with_ssh_config(program: &str) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    if let Some(identity_file) = get_env("SSH_IDENTITY_FILE") {
        command.arg("-i").arg(identity_file);
    }
    if let Some(known_hosts_file) = get_env("SSH_KNOWN_HOSTS_FILE") {
        command
            .arg("-o")
            .arg(format!("UserKnownHostsFile={known_hosts_file}"));
    }
    if let Some(policy) = get_env("SSH_STRICT_HOST_KEY_CHECKING") {
        command
            .arg("-o")
            .arg(format!("StrictHostKeyChecking={policy}"));
    }
    command
}

/// the environment variable `name`, unless unset or empty
fn get_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// `words` (a program and its arguments) as one command line, with each word quoted so the remote
/// shell can neither split nor expand it.
///
/// `ssh` joins the command arguments it gets with spaces, and a shell on the remote machine splits
/// and expands the result again. So passing them as separate arguments quotes nothing: an app spec
/// under a path with a space would arrive as two arguments, and a `;`, a backtick or `$(…)` in a
/// tag or a command payload would run as a command on the remote machine. [`crate::myrmic::Local`]
/// doesn't have this problem because it runs the program directly; SSH is the only path with a
/// shell in it.
fn quoted_command_line(words: &[&str]) -> String {
    words
        .iter()
        // Single quotes suppress every expansion; the one character they cannot carry is a single
        // quote itself, which is closed, backslash-escaped and reopened.
        .map(|word| format!("'{}'", word.replace('\'', r"'\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// the hex SHA-256 of the file at `path`, as `sha256sum` prints it
fn sha256_of(path: &Path) -> String {
    let mut file = std::fs::File::open(path)
        .unwrap_or_else(|e| panic!("failed to open {}: {e}", path.display()));
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::quoted_command_line;

    #[test]
    fn a_path_with_a_space_stays_one_argument() {
        assert_eq!(
            quoted_command_line(&["myrmic", "deploy", "/home/user/my benchmarks/app.yml"]),
            "'myrmic' 'deploy' '/home/user/my benchmarks/app.yml'",
        );
    }

    #[test]
    fn shell_metacharacters_do_not_reach_the_remote_shell() {
        // Unquoted, the remote shell would run `id` and `rm -rf /` as commands
        // of their own on the rack node.
        assert_eq!(
            quoted_command_line(&["myrmic", "send", "cell/one", "go; rm -rf / $(id)"]),
            "'myrmic' 'send' 'cell/one' 'go; rm -rf / $(id)'",
        );
    }

    #[test]
    fn a_single_quote_is_closed_escaped_and_reopened() {
        assert_eq!(
            quoted_command_line(&["myrmic", "send", "it's"]),
            r"'myrmic' 'send' 'it'\''s'",
        );
    }
}
