use crate::docker::container::ConnectedContainer;

use super::MyrmicBackend;

/// [`MyrmicBackend`] that runs the myrmic CLI inside a docker container that has `myrmic` on its
/// `PATH`.
///
/// It only borrows the container and never removes anything: whoever created the container owns
/// it (e.g. a [`crate::docker::managed::ManagedContainer`], removed on drop), and the borrow makes
/// "the container outlives every [`super::super::Myrmic`] and guard using it" a compile-time rule.
/// The cost is that neither is `'static`. Should something need that, the way out is an
/// `OwningDockerBinary { container: Arc<ManagedContainer> }` that removes the container when its
/// last clone drops.
#[derive(Clone, Copy)]
pub struct DockerBinary<'c> {
    container: &'c ConnectedContainer,
}

impl<'c> DockerBinary<'c> {
    /// run myrmic inside `container`
    pub fn attach(container: &'c ConnectedContainer) -> Self {
        Self { container }
    }

    /// the container the myrmic CLI is executed in
    pub fn container(&self) -> &'c ConnectedContainer {
        self.container
    }
}

impl MyrmicBackend for DockerBinary<'_> {
    fn binary(&self) -> &'static str {
        "myrmic"
    }

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
}
