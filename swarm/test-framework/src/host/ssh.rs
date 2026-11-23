//! Machines the test reaches over SSH: a machine to run runtimes and routers on ([`Ssh`],
//! exported as [`super::Ssh`]), and waits for a machine to be back after a reboot, which need no
//! more of it than its SSH destination ([`wait_until_reachable`],
//! [`wait_until_clock_synchronized`]).

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

/// A machine the test reaches over SSH, e.g. a Raspberry Pi of the rack. It exists on its own,
/// before any host set: host sets take it in, and give it its address there. Clones are the same
/// host.
///
/// Every SSH connection to it takes these options from the environment where set: the identity
/// file (`SSH_IDENTITY_FILE`), the known hosts file (`SSH_KNOWN_HOSTS_FILE`, e.g. the one a CI
/// setup prepared, rather than `ssh`'s default) and the host key policy
/// (`SSH_STRICT_HOST_KEY_CHECKING`, e.g. `accept-new` in a CI job whose fresh container knows no
/// host yet).
#[derive(Clone)]
pub struct Ssh {
    /// SSH destination, e.g. `user@rack-node-1.example`: anything `ssh` accepts (host aliases from
    /// `~/.ssh/config` included); it also names the host in messages
    destination: Arc<str>,
    /// the directory the machine's `myrmic` and `zenoh-router` lie in (the test puts them there,
    /// see [`Self::upload`])
    bin_dir: Arc<str>,
    shared: Arc<Shared>,
}

impl Ssh {
    /// The machine at the SSH destination `destination`, with its `myrmic` and `zenoh-router` in
    /// `bin_dir`, nothing in its firewall and in no host set yet.
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
    /// it has state directories of its own, created on the machine here and removed there with the
    /// last clone of the shim, so its runtimes see no name, identity or data of an earlier run.
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

    /// A zenoh router on this machine, a new member of an overlay attached by `attachment`.
    /// Returns once it listens, so a swarm can attach through it right away (see [`Router`]).
    pub async fn router(&self, attachment: impl Into<overlay::Attachment>) -> Router<Self> {
        let member = attachment.into().add_member(self.get_id());
        let binary = format!("{}/zenoh-router", self.bin_dir);
        Router::start(self.clone(), &binary, member).await
    }

    /// Copy the local binary `local` into the machine's binary directory (see [`Self::new`])
    /// under its own name, and have it on the machine's disk before returning, so a test that cuts
    /// the machine's power doesn't lose it. Copies nothing if the machine already has a file with
    /// the same content there. Panics if it fails.
    ///
    /// For several machines, upload to all at once, e.g. with `futures::future::join_all`.
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

/// Wait until the machine at the SSH destination `destination` takes SSH connections, e.g. after
/// it rebooted. Panics after `timeout`. Connects with the options from the environment a [`Ssh`]
/// does.
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
/// after `timeout`. Connects with the options from the environment a [`Ssh`] does.
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

/// Run `words` on the machine at `destination` until `done` says its output is the awaited one.
/// Panics after `timeout`, saying what the machine failed `to` do and showing the last output.
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
/// every SSH connection shares (see [`Ssh`]). Each is read fresh on every call, so a test that
/// sets one mid-run sees it take effect.
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

/// `words` (a program and its arguments) as one command line, each word quoted so the remote
/// shell can neither split nor expand it.
///
/// `ssh` joins the command arguments it is handed with spaces and gives the
/// result to a shell on the far side, which splits and expands it all over
/// again — so passing them as separate argv entries quotes nothing. An app
/// spec under a path with a space arrives as two arguments, and a `;`,
/// backtick or `$(…)` in a tag or a command payload runs as a command on
/// the rack node. [`crate::myrmic::Local`] is unaffected because it
/// execs directly; SSH is the only path with a shell in it.
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
