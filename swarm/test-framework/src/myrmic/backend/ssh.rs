use super::MyrmicBackend;

/// [`MyrmicBackend`] that runs programs on a remote host over SSH.
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
}

impl Ssh {
    /// run programs on `host`
    pub fn new(host: impl Into<String>) -> Self {
        Self { host: host.into() }
    }

    /// the SSH destination this backend targets
    pub fn host(&self) -> &str {
        &self.host
    }
}

impl MyrmicBackend for Ssh {
    /// `ssh [-i identity] [-o UserKnownHostsFile=..] <host> <quoted remote command>`
    fn command(&self, program: &str, args: &[&str]) -> std::process::Command {
        let mut command = crate::ssh_command();
        command
            .arg(&self.host)
            .arg(crate::remote_command(program, args));
        command
    }

    fn wrapper_exit_codes(&self) -> &'static [i32] {
        // 255: `ssh` itself failed (host unreachable, authentication); 127: the remote shell
        // found no such program
        &[127, 255]
    }

    fn runtime_config(&self) -> Option<String> {
        None
    }
}
