use super::MyrmicBackend;

/// [`MyrmicBackend`] that runs the myrmic CLI on a remote host over SSH.
///
/// Every command shells out to the system `ssh` binary (`ssh <host> <program> <args>...`) rather
/// than using an SSH client crate, like [`super::docker::DockerBinary`] shells out to the system
/// `docker` binary: [`MyrmicBackend::command`] is a plain process on the host, so the async and
/// the blocking (Drop-guard) paths run the same invocation.
#[derive(Clone)]
pub struct SshBinary {
    /// SSH destination, e.g. `user@rack-node-1.example` - anything `ssh` itself accepts
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
}

impl MyrmicBackend for SshBinary {
    fn binary(&self) -> &str {
        &self.binary
    }

    /// `ssh [-i identity] [-o UserKnownHostsFile=..] <host> <quoted remote command>`
    fn command(&self, program: &str, args: &[&str]) -> std::process::Command {
        let mut command = std::process::Command::new("ssh");
        if let Some(identity_file) = crate::ssh_identity_file() {
            command.arg("-i").arg(identity_file);
        }
        if let Some(known_hosts_file) = crate::ssh_known_hosts_file() {
            command
                .arg("-o")
                .arg(format!("UserKnownHostsFile={known_hosts_file}"));
        }
        command.arg(&self.host).arg(remote_command(program, args));
        command
    }

    fn wrapper_exit_codes(&self) -> &'static [i32] {
        // 255: `ssh` itself failed (host unreachable, authentication); 127: the remote shell
        // found no such program
        &[127, 255]
    }
}

/// The remote invocation of `program args` as a single, fully quoted shell
/// word sequence.
///
/// `ssh` joins the command arguments it is handed with spaces and gives the
/// result to a shell on the far side, which splits and expands it all over
/// again — so passing them as separate argv entries quotes nothing. An app
/// spec under a path with a space arrives as two arguments, and a `;`,
/// backtick or `$(…)` in a tag or a command payload runs as a command on
/// the rack node. [`super::local::LocalBinary`] is unaffected because it
/// execs directly; this is the only path with a shell in it.
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

#[cfg(test)]
mod tests {
    use super::remote_command;

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
