//! Preparing remote hosts for a swarm that runs on them over SSH (see
//! [`crate::swarm2::backend::ssh::Ssh`]), and the SSH helpers every SSH connection the
//! test-framework opens shares.

mod upload;

pub use upload::upload;

/// A binary on a remote host: how to reach the host, and where the binary lies there.
#[derive(Debug, Clone)]
pub struct RemoteBinary {
    /// SSH destination, e.g. `user@host.example`: anything `ssh` and `scp` accept
    pub login: String,
    /// path of the binary the host runs (`myrmic`, or `zenoh-router` on the entry host)
    pub remote_bin_path: String,
}

/// Optional SSH identity file (`ssh -i`/`scp -i`) applied to every SSH/SCP connection the
/// test-framework opens (the myrmic-over-SSH backend, rack provisioning) — set `SSH_IDENTITY_FILE`
/// when the target hosts require a non-default key. Read fresh on every call (not cached) so
/// tests that set the env var mid-run see the change take effect.
#[must_use]
pub(crate) fn ssh_identity_file() -> Option<String> {
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
pub(crate) fn ssh_known_hosts_file() -> Option<String> {
    std::env::var("SSH_KNOWN_HOSTS_FILE")
        .ok()
        .filter(|value| !value.is_empty())
}

/// `program` (`ssh` or `scp`, which take the same options) with the options every SSH connection
/// the test-framework opens shares: [`ssh_identity_file`] and [`ssh_known_hosts_file`], where set.
pub(crate) fn command_with_ssh_config(program: &str) -> std::process::Command {
    let mut command = std::process::Command::new(program);
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
pub(crate) fn quoted_command_line(words: &[&str]) -> String {
    words
        .iter()
        // Single quotes suppress every expansion; the one character they cannot carry is a single
        // quote itself, which is closed, backslash-escaped and reopened.
        .map(|word| format!("'{}'", word.replace('\'', r"'\''")))
        .collect::<Vec<_>>()
        .join(" ")
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
