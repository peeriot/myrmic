use std::sync::Arc;

use super::MyrmicBackend;
use crate::{
    host::{self, Remote},
    run_cleanup,
};

/// A [`MyrmicBackend`] that runs programs on an SSH host ([`host::Ssh`]), isolated: with state
/// directories of their own there (see [`host::Ssh::myrmic_isolated`]).
///
/// Every command runs the system `ssh` binary (`ssh <host> env XDG_DATA_HOME=.. XDG_RUNTIME_DIR=..
/// <program> <args>...`, each word quoted) instead of using an SSH client crate, just as [`super::docker::Docker`] runs the system `docker` binary. So
/// [`MyrmicBackend::command`] is a plain process on the test's machine, and the async path and the
/// blocking path (in `Drop` guards) run the same command.
#[derive(Clone)]
pub struct Ssh {
    host: host::Ssh,
    /// shared by the clones, so its directories live as long as any runtime or cell handle that
    /// may still need them for cleanup
    isolation: Arc<Isolation>,
}

/// A private myrmic setup on an SSH host (see [`Ssh::isolated`]). Dropping it removes its
/// directories there.
struct Isolation {
    host: host::Ssh,
    /// `XDG_DATA_HOME` on the host (runtime identities, databases, logs), on its disk, so a
    /// runtime keeps its data across a reboot of the host
    data_dir: String,
    /// `XDG_RUNTIME_DIR` on the host, holding the pid files that `runtimes list` and `delete` read.
    /// It is on the host's tmpfs, like the default one, so a reboot removes the pid files together
    /// with the processes they name: no pid file outlives its process and names another process
    /// after the reboot.
    run_dir: String,
}

impl Ssh {
    /// Run programs on `host` with state directories of their own, created on the host here. A
    /// runtime started this way sees no name, identity or data of any other runtime.
    pub(crate) fn isolated(host: host::Ssh) -> Self {
        // `mktemp` prints the directory it created
        let create = r#"mktemp -d "$HOME/myrmic-isolated.XXXXXX" && mktemp -d "${XDG_RUNTIME_DIR:?}/myrmic-isolated.XXXXXX""#;
        let output = host
            .command("sh", &["-c", create])
            .output()
            .unwrap_or_else(|err| panic!("failed to run ssh: {err}"));
        assert!(
            output.status.success(),
            "failed to create the isolation's directories on {}: {}",
            host.get_destination(),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut dirs = stdout.lines();
        let data_dir = dirs
            .next()
            .expect("the first `mktemp` printed its directory")
            .to_owned();
        let run_dir = dirs
            .next()
            .expect("the second `mktemp` printed its directory")
            .to_owned();
        Self {
            isolation: Arc::new(Isolation {
                host: host.clone(),
                data_dir,
                run_dir,
            }),
            host,
        }
    }

    /// the host programs run on
    pub fn get_host(&self) -> &host::Ssh {
        &self.host
    }
}

impl Drop for Isolation {
    fn drop(&mut self) {
        let remove = self
            .host
            .ssh(&["rm", "-rf", "--", &self.data_dir, &self.run_dir]);
        run_cleanup("Isolation", remove);
    }
}

impl MyrmicBackend for Ssh {
    /// `ssh <options> <host> <quoted remote command>` (see [`host::Ssh`]), with the isolation's
    /// state directories as `XDG_DATA_HOME` and `XDG_RUNTIME_DIR`, set by `env` on the host
    fn command(&self, program: &str, args: &[&str]) -> std::process::Command {
        let data_home = format!("XDG_DATA_HOME={}", self.isolation.data_dir);
        let runtime_dir = format!("XDG_RUNTIME_DIR={}", self.isolation.run_dir);
        let mut env_args = vec![data_home.as_str(), runtime_dir.as_str(), program];
        env_args.extend(args);
        self.host.command("env", &env_args)
    }

    fn wrapper_exit_codes(&self) -> &'static [i32] {
        // 255: `ssh` itself failed (host unreachable, authentication); 127: the remote shell
        // found no such program
        &[127, 255]
    }

    fn get_host_id(&self) -> host::Id {
        self.host.get_id()
    }
}
