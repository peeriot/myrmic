use crate::docker::container::ConnectedContainer;

use super::MyrmicBackend;

/// [`MyrmicBackend`] that runs programs inside a docker container.
///
/// It only borrows the container and never removes anything: whoever created the container owns
/// it (e.g. a [`crate::docker::managed::ManagedContainer`], removed on drop), and the borrow makes
/// "the container outlives every [`super::super::Myrmic`] and guard using it" a compile-time rule.
/// The cost is that neither is `'static`. Should something need that, the way out is an
/// `OwningDocker { container: Arc<ManagedContainer> }` that removes the container when its last
/// clone drops.
#[derive(Clone, Copy)]
pub struct Docker<'c> {
    container: &'c ConnectedContainer,
}

impl<'c> Docker<'c> {
    /// run programs inside `container`
    pub fn attach(container: &'c ConnectedContainer) -> Self {
        Self { container }
    }

    /// the container programs are executed in
    pub fn container(&self) -> &'c ConnectedContainer {
        self.container
    }
}

impl MyrmicBackend for Docker<'_> {
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
