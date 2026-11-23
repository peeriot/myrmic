//! Where the myrmic CLI runs: on the test's machine, on a remote machine over SSH, or inside a
//! docker container.

use crate::host;

mod docker;
mod local;
mod ssh;

pub use docker::Docker;
pub use local::Local;
pub use ssh::Ssh;

/// Where and how the myrmic CLI runs.
///
/// A backend only builds the commands. [`super::Myrmic`] runs them and holds all knowledge of the
/// CLI's commands and output, so the backends can't drift apart.
pub trait MyrmicBackend: Clone + Send + Sync {
    /// `program args` as a command on the host that runs them on the target: the program itself,
    /// or the `ssh`/`docker exec` invocation around it
    fn command(&self, program: &str, args: &[&str]) -> std::process::Command;

    /// The exit codes that come from the wrapper around the program rather than from the program
    /// (`ssh` couldn't connect, `docker exec` couldn't reach the daemon, the program is missing on
    /// the target). They mean the test infrastructure is broken, which is not a result.
    fn wrapper_exit_codes(&self) -> &'static [i32];

    /// the host the commands run on, as its network knows it
    fn get_host_id(&self) -> host::Id;
}
