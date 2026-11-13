use super::MyrmicBackend;
use crate::network::HostId;

/// [`MyrmicBackend`] that runs programs on a remote host over SSH, on the host's own state
/// directories.
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
        let mut words = vec![program];
        words.extend(args);
        let mut command = crate::ssh_utils::command_with_ssh_config("ssh");
        command
            .arg(&self.host)
            .arg(crate::ssh_utils::quoted_command_line(&words));
        command
    }

    fn wrapper_exit_codes(&self) -> &'static [i32] {
        // 255: `ssh` itself failed (host unreachable, authentication); 127: the remote shell
        // found no such program
        &[127, 255]
    }

    fn get_host_id(&self) -> HostId {
        // the SSH host, with its network, comes with the rack's redesign
        todo!()
    }
}
