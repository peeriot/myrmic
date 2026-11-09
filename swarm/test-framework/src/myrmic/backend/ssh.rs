use std::sync::Arc;

use super::{MyrmicBackend, isolated_runtime_config};

/// How long the cleanup of an [`Isolation`] waits for its host to answer: one that lost power
/// may not be back.
const CLEANUP_CONNECT_TIMEOUT: &str = "ConnectTimeout=5";

/// [`MyrmicBackend`] that runs programs on a remote host over SSH, on the host's own state
/// directories or, for the SSH swarm backend, isolated like [`super::Local`]: on state directories
/// and a multicast group of its own.
///
/// Every command shells out to the system `ssh` binary (`ssh <host> <program> <args>...`) rather
/// than using an SSH client crate, like [`super::docker::Docker`] shells out to the system
/// `docker` binary: [`MyrmicBackend::command`] is a plain process on the host, so the async and
/// the blocking (Drop-guard) paths run the same invocation.
#[derive(Clone)]
pub struct Ssh {
    /// SSH destination, e.g. `user@rack-node-1.example` - anything `ssh` itself accepts
    /// (host aliases from `~/.ssh/config` included).
    host: String,
    /// `None` for the host's own state directories and the default multicast group; shared by
    /// the clones, so its directories live as long as any runtime handle that may still need
    /// them for cleanup
    isolation: Option<Arc<Isolation>>,
}

/// A private myrmic setup on a remote host (see [`Ssh::isolated`]). Dropping it removes its
/// directories there.
struct Isolation {
    host: String,
    /// `XDG_DATA_HOME` on the host (runtime identities, databases, logs), on its disk, so a
    /// runtime keeps its data across a reboot of the host
    data_dir: String,
    /// `XDG_RUNTIME_DIR` on the host (the pid files `runtimes list` and `delete` read), on the
    /// host's tmpfs like the default one: a reboot takes the pid files along with the processes
    /// they name, so none outlives its process and names another one after the reboot
    run_dir: String,
    /// the multicast group (`<address>:<port>`) its runtimes scout on
    multicast_group: String,
}

impl Ssh {
    /// run programs on `host`
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            isolation: None,
        }
    }

    /// Run programs on `host` with state directories of their own, created on the host here,
    /// and on `multicast_group` (`<address>:<port>`), which the runtimes of one swarm share. A
    /// runtime started this way sees no runtime name, identity or data of any other.
    pub(crate) fn isolated(host: impl Into<String>, multicast_group: String) -> Self {
        let host = host.into();
        // `mktemp` prints the directory it created
        let create = r#"mktemp -d "$HOME/myrmic-isolated.XXXXXX" && mktemp -d "${XDG_RUNTIME_DIR:?}/myrmic-isolated.XXXXXX""#;
        let output = ssh(&host, &["sh", "-c", create], &[])
            .output()
            .unwrap_or_else(|e| panic!("failed to run ssh: {e}"));
        assert!(
            output.status.success(),
            "failed to create the isolation's directories on {host}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut dirs = stdout.lines();
        let data_dir = dirs
            .next()
            .expect("the first `mktemp` printed its directory");
        let run_dir = dirs
            .next()
            .expect("the second `mktemp` printed its directory");
        let isolation = Isolation {
            host: host.clone(),
            data_dir: data_dir.to_owned(),
            run_dir: run_dir.to_owned(),
            multicast_group,
        };
        Self {
            host,
            isolation: Some(Arc::new(isolation)),
        }
    }

    /// the SSH destination this backend targets
    pub fn host(&self) -> &str {
        &self.host
    }
}

impl Drop for Isolation {
    fn drop(&mut self) {
        let removed = ssh(
            &self.host,
            &["rm", "-rf", "--", &self.data_dir, &self.run_dir],
            &["-o", "BatchMode=yes", "-o", CLEANUP_CONNECT_TIMEOUT],
        )
        .output();
        match removed {
            Ok(output) if output.status.success() => {}
            Ok(output) => eprintln!(
                "Isolation drop-guard: failed to remove {} and {} on {}: {}",
                self.data_dir,
                self.run_dir,
                self.host,
                String::from_utf8_lossy(&output.stderr)
            ),
            Err(err) => eprintln!(
                "Isolation drop-guard: failed to run the removal on {}: {err}",
                self.host
            ),
        }
    }
}

impl MyrmicBackend for Ssh {
    /// `ssh [-i identity] [-o UserKnownHostsFile=..] <host> <quoted remote command>`, in an
    /// isolation with its state directories as `XDG_DATA_HOME` and `XDG_RUNTIME_DIR` and its
    /// multicast group as `DEFAULT_MYRMIC_MULTICAST_GROUP`, set by `env` on the host
    fn command(&self, program: &str, args: &[&str]) -> std::process::Command {
        let environment = self.isolation.as_deref().map(|isolation| {
            [
                format!("XDG_DATA_HOME={}", isolation.data_dir),
                format!("XDG_RUNTIME_DIR={}", isolation.run_dir),
                format!(
                    "DEFAULT_MYRMIC_MULTICAST_GROUP={}",
                    isolation.multicast_group
                ),
            ]
        });
        let mut words: Vec<&str> = vec![];
        if let Some(environment) = &environment {
            words.push("env");
            words.extend(environment.iter().map(String::as_str));
        }
        words.push(program);
        words.extend(args);
        ssh(&self.host, &words, &[])
    }

    fn wrapper_exit_codes(&self) -> &'static [i32] {
        // 255: `ssh` itself failed (host unreachable, authentication); 127: the remote shell
        // found no such program
        &[127, 255]
    }

    /// in an isolation, the config that lets the runtime scout on the isolation's multicast group
    fn runtime_config(&self) -> Option<String> {
        self.isolation
            .as_deref()
            .map(|isolation| isolated_runtime_config(&isolation.multicast_group))
    }
}

/// `ssh [options] <host> <words>`, the words quoted so the remote shell runs them as they are
fn ssh(host: &str, words: &[&str], options: &[&str]) -> std::process::Command {
    let mut command = crate::ssh_utils::command_with_ssh_config("ssh");
    command
        .args(options)
        .arg(host)
        .arg(crate::ssh_utils::quoted_command_line(words));
    command
}
