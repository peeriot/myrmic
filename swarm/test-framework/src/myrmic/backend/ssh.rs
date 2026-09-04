use crate::docker::CommandOutput;
use crate::myrmic::cell::CellSpec;

use super::{MyrmicBackend, parse_runtime_list, parse_status_lines};

/// [`MyrmicBackend`] that runs the myrmic CLI on a remote host over SSH.
///
/// Every method shells out to the system `ssh` binary (`ssh <host> <binary> <args>...`) rather
/// than using an SSH client crate, mirroring how [`super::docker::DockerBinary`] shells out to
/// the system `docker` binary for its blocking Drop-guard path — there's no SSH crate already in
/// the workspace, and a single dependency-free code path is simpler to reason about than mixing
/// an async SSH client with a sync one for the blocking variants.
#[derive(Clone)]
pub struct SshBinary {
    /// SSH destination, e.g. `peeriot@rack-node-3.peeriot.intra` — anything `ssh` itself accepts
    /// (host aliases from `~/.ssh/config` included).
    host: String,
    /// path to the myrmic binary on the remote host (defaults to `myrmic`, i.e. resolved via the
    /// remote user's `PATH`).
    binary: String,
}

impl SshBinary {
    /// wrap the myrmic binary reachable via `ssh host myrmic ...`, resolving `myrmic` on the
    /// remote `PATH`
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            binary: "myrmic".to_owned(),
        }
    }

    /// like [`Self::new`], but the remote myrmic binary lives at `binary` rather than on `PATH`
    /// (e.g. a path a benchmark harness `scp`'d it to)
    pub fn at(host: impl Into<String>, binary: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            binary: binary.into(),
        }
    }

    /// the SSH destination this backend targets
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The remote myrmic invocation as a single, fully quoted shell word
    /// sequence.
    ///
    /// `ssh` joins the command arguments it is handed with spaces and gives the
    /// result to a shell on the far side, which splits and expands it all over
    /// again — so passing them as separate argv entries quotes nothing. An app
    /// spec under a path with a space arrives as two arguments, and a `;`,
    /// backtick or `$(…)` in a tag or a command payload runs as a command on
    /// the rack node. [`super::local::LocalBinary`] is unaffected because it
    /// execs directly; this is the only path with a shell in it.
    fn remote_command(&self, args: &[&str]) -> String {
        std::iter::once(self.binary.as_str())
            .chain(args.iter().copied())
            .map(shell_quote)
            .collect::<Vec<_>>()
            .join(" ")
    }

    async fn exec(&self, args: &[&str]) -> CommandOutput {
        let cmd = self.remote_command(args);
        let identity_file = crate::ssh_identity_file();
        let mut command = tokio::process::Command::new("ssh");
        if let Some(identity_file) = &identity_file {
            command.arg("-i").arg(identity_file);
        }
        if let Some(known_hosts_file) = crate::ssh_known_hosts_file() {
            command
                .arg("-o")
                .arg(format!("UserKnownHostsFile={known_hosts_file}"));
        }
        let output = command
            .arg(&self.host)
            .arg(&cmd)
            .output()
            .await
            .unwrap_or_else(|e| panic!("failed to run ssh {} {cmd}: {e}", self.host));
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        // `expect_success` only ever prints `stderr` (not the command that produced it), so fold
        // in the identity file/host/exit code here — otherwise a failure (e.g. "Host key
        // verification failed") gives no way to tell which host or SSH invocation it came from.
        let stderr = if output.status.success() {
            stderr
        } else {
            format!(
                "ssh {}{} {cmd} (exit {:?}): {stderr}",
                identity_file
                    .as_deref()
                    .map(|f| format!("-i {f} "))
                    .unwrap_or_default(),
                self.host,
                output.status.code()
            )
        };
        CommandOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr,
        }
    }

    /// Drop-only best-effort helper — used by the blocking delete variants; do not use for
    /// happy-path operations.
    fn exec_blocking(&self, myrmic_args: &[&str]) -> Result<(), String> {
        let identity_file = crate::ssh_identity_file();
        let known_hosts_file = crate::ssh_known_hosts_file();
        let known_hosts_opt = known_hosts_file
            .as_deref()
            .map(|f| format!("UserKnownHostsFile={f}"));
        let mut args = Vec::new();
        if let Some(identity_file) = &identity_file {
            args.push("-i");
            args.push(identity_file.as_str());
        }
        if let Some(known_hosts_opt) = &known_hosts_opt {
            args.push("-o");
            args.push(known_hosts_opt.as_str());
        }
        args.push(self.host.as_str());
        let cmd = self.remote_command(myrmic_args);
        args.push(cmd.as_str());
        let output = std::process::Command::new("ssh")
            .args(&args)
            .output()
            .map_err(|e| format!("failed to run ssh {args:?}: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "ssh {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ))
        }
    }
}

/// One argument as a single shell word the remote shell cannot split or expand.
///
/// Single quotes suppress every expansion; the one character they cannot carry
/// is a single quote itself, which is closed, backslash-escaped and reopened.
fn shell_quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', r"'\''"))
}

impl MyrmicBackend for SshBinary {
    async fn send(&self, sri: &str, command: &str) -> Option<String> {
        let output = self
            .exec(&["send", sri, command])
            .await
            .expect_success(&format!("myrmic send {sri}/{command}"));
        let stdout = output.stdout.trim().to_owned();
        if stdout.is_empty() {
            None
        } else {
            Some(stdout)
        }
    }

    async fn start_runtime_at(&self, name: &str, tags: &[&str], config_path: Option<&str>) {
        // Unlike local/docker callers (which pick a fresh random runtime name per test run, so
        // each gets its own never-reused db directory automatically), rack deployments use
        // stable, predictable names (e.g. "central", "zone-0") so re-running with the same
        // topology doesn't require picking new ones — but `myrmic runtimes start --name` reuses
        // the same on-disk db across runs for a stable name, so a second run would otherwise
        // collide on the previous run's registered cells (`DuplicateSri`). `--tmp` keeps the db
        // in memory, discarded when the runtime stops, matching the "fresh every run" behavior
        // the local/docker paths get for free from their random names.
        let mut args = vec!["runtimes", "start", "-d", "--tmp", "--name", name];
        for tag in tags.iter().copied() {
            args.push("--tag");
            args.push(tag);
        }
        if let Some(path) = config_path {
            args.push(path);
        }
        self.exec(&args).await.expect_success("runtimes start");
    }

    async fn delete_runtime(&self, name: &str) {
        self.exec(&["runtimes", "delete", name])
            .await
            .expect_success("runtimes delete");
    }

    async fn list_runtimes(&self) -> Vec<String> {
        let output = self
            .exec(&["runtimes", "list"])
            .await
            .expect_success("runtimes list");
        parse_runtime_list(&output.stdout)
    }

    async fn new_cell(&self, path: &std::path::Path, name: &str, sdk: Option<&str>) {
        let path = path.display().to_string();

        let mut options = vec!["new"];
        if let Some(sdk) = sdk {
            options.extend_from_slice(&["--sdk", sdk]);
        }
        options.extend_from_slice(&["--name", name, path.as_str()]);

        self.exec(&options).await.expect_success("new");
    }

    async fn deploy(&self, cell: CellSpec, sri: &str, tags: &[&str]) {
        let cell_path = cell.as_path().display().to_string();
        let mut args = vec!["deploy", "--sri", sri, cell_path.as_str()];
        for tag in tags.iter().copied() {
            args.push("--tag");
            args.push(tag);
        }
        self.exec(&args).await.expect_success("deploy");
    }

    async fn deploy_app(&self, app_spec: &std::path::Path) {
        let app_spec_path = app_spec.display().to_string();
        self.exec(&["deploy", app_spec_path.as_str()])
            .await
            .expect_success("deploy app");
    }

    async fn delete_cell(&self, sri: &str) {
        self.exec(&["delete", sri]).await.expect_success("delete");
    }

    async fn status(&self) -> Vec<String> {
        let output = self
            .exec(&["cells", "status"])
            .await
            .expect_success("runtimes list");
        parse_status_lines(&output.stdout)
    }

    fn delete_runtime_blocking(&self, name: &str) -> Result<(), String> {
        self.exec_blocking(&["runtimes", "delete", name])
    }

    fn delete_cell_blocking(&self, sri: &str) -> Result<(), String> {
        self.exec_blocking(&["delete", sri])
    }
}

#[cfg(test)]
mod tests {
    use super::SshBinary;

    #[test]
    fn a_path_with_a_space_stays_one_argument() {
        let ssh = SshBinary::new("rack-node-1");

        assert_eq!(
            ssh.remote_command(&["deploy", "/home/peeriot/my benchmarks/app.yml"]),
            "'myrmic' 'deploy' '/home/peeriot/my benchmarks/app.yml'",
        );
    }

    #[test]
    fn shell_metacharacters_do_not_reach_the_remote_shell() {
        let ssh = SshBinary::new("rack-node-1");

        // Unquoted, the remote shell would run `id` and `rm -rf /` as commands
        // of their own on the rack node.
        assert_eq!(
            ssh.remote_command(&["send", "cell/one", "go; rm -rf / $(id)"]),
            "'myrmic' 'send' 'cell/one' 'go; rm -rf / $(id)'",
        );
    }

    #[test]
    fn a_single_quote_is_closed_escaped_and_reopened() {
        let ssh = SshBinary::new("rack-node-1");

        assert_eq!(
            ssh.remote_command(&["send", "it's"]),
            r"'myrmic' 'send' 'it'\''s'",
        );
    }
}
