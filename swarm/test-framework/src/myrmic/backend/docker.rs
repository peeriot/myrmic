use std::sync::Arc;

use crate::docker::container::{ConnectedContainer, ManagedContainer};

use super::MyrmicBackend;

/// [`MyrmicBackend`] that runs programs inside a docker container.
///
/// It shares ownership of the container with its clones, so the container outlives every
/// [`super::super::Myrmic`] and guard using it, and is removed when the last of them drops.
#[derive(Clone)]
pub struct Docker {
    container: Arc<ManagedContainer>,
}

impl Docker {
    /// run programs inside `container`
    pub fn attach(container: Arc<ManagedContainer>) -> Self {
        Self { container }
    }

    /// the container programs are executed in
    pub fn container(&self) -> &ConnectedContainer {
        &self.container
    }
}

impl MyrmicBackend for Docker {
    fn command(&self, program: &str, args: &[&str]) -> std::process::Command {
        let mut command = self.container.docker_cli();
        command
            .args(["exec", self.container.id(), program])
            .args(args);
        command
    }

    fn wrapper_exit_codes(&self) -> &'static [i32] {
        // 125: `docker` itself failed (daemon unreachable, no such container); 126: the program
        // cannot be invoked; 127: the program does not exist in the container
        &[125, 126, 127]
    }

    fn runtime_config(&self) -> Option<String> {
        None
    }
}
