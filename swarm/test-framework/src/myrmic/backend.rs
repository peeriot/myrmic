//! Where the myrmic CLI runs: on the host, on a remote host over SSH, or inside a docker
//! container.

mod docker;
mod local;
mod ssh;

pub use docker::Docker;
pub use local::Local;
pub use ssh::Ssh;

/// Where and how the myrmic CLI is executed.
///
/// A backend only builds host commands. [`super::Myrmic`] runs them and holds all knowledge of
/// the CLI's commands and output, so the backends cannot drift apart.
pub trait MyrmicBackend: Clone + Send + Sync {
    /// `program args` as a command on the host that runs them on the target: the program itself,
    /// or the `ssh`/`docker exec` invocation around it
    fn command(&self, program: &str, args: &[&str]) -> std::process::Command;

    /// Exit codes that belong to the wrapper around the program (`ssh` could not connect,
    /// `docker exec` could not reach the daemon, the program is missing on the target) rather
    /// than to the program: broken test infrastructure, not a result.
    fn wrapper_exit_codes(&self) -> &'static [i32];
}
