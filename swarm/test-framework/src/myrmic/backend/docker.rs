use crate::host::{self, Remote};

use super::MyrmicBackend;

/// A [`MyrmicBackend`] that runs programs in the container of a docker host.
///
/// It holds its host, whose clones share the container. So the container outlives every
/// [`crate::myrmic::Myrmic`] and guard that uses it, and is removed when the last of them is
/// dropped.
///
/// `S` is the host's storage ([`host::Tmpfs`] or [`host::Volume`]).
pub struct Docker<S = host::Tmpfs> {
    host: host::Docker<S>,
}

impl<S> Docker<S> {
    /// run programs in `host`'s container
    pub(crate) fn new(host: host::Docker<S>) -> Self {
        Self { host }
    }

    /// the host whose container programs run in
    pub fn get_host(&self) -> &host::Docker<S> {
        &self.host
    }
}

impl<S> Clone for Docker<S> {
    fn clone(&self) -> Self {
        Self {
            host: self.host.clone(),
        }
    }
}

impl<S> MyrmicBackend for Docker<S> {
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
