use crate::host::{self, Remote};

use super::MyrmicBackend;

/// [`MyrmicBackend`] that runs programs in a docker host's container.
///
/// It holds its host, which shares the container with its clones, so the container outlives every
/// [`super::super::Myrmic`] and guard using it, and is removed when the last of them drops.
#[derive(Clone)]
pub struct Docker {
    host: host::Docker,
}

impl Docker {
    /// run programs in `host`'s container
    pub(crate) fn new(host: host::Docker) -> Self {
        Self { host }
    }

    /// the host whose container programs run in
    pub fn get_host(&self) -> &host::Docker {
        &self.host
    }
}

impl MyrmicBackend for Docker {
    fn command(&self, program: &str, args: &[&str]) -> std::process::Command {
        self.host.command(program, args)
    }

    fn wrapper_exit_codes(&self) -> &'static [i32] {
        // 125: `docker` itself failed (daemon unreachable, no such container); 126: the program
        // cannot be invoked; 127: the program does not exist in the container
        &[125, 126, 127]
    }

    fn get_host_id(&self) -> host::Id {
        self.host.get_id()
    }
}
