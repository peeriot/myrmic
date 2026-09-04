//! Thin docker wrappers (images, containers, network shaping) on top of [`bollard`].

pub use bollard::Docker;

pub mod container;
pub mod image;
pub mod managed;

/// Captured output of a command executed inside a container.
#[derive(Debug, Clone)]
pub struct CommandOutput {
    /// whether the command exited with code 0
    pub success: bool,
    /// captured stdout, lossily decoded as UTF-8
    pub stdout: String,
    /// captured stderr, lossily decoded as UTF-8
    pub stderr: String,
}

impl CommandOutput {
    /// Panics with `what` and the captured stderr if the command didn't exit successfully,
    /// otherwise returns `self` for chaining.
    pub fn expect_success(self, what: &str) -> Self {
        if !self.success {
            eprintln!("{}", self.stderr);
            panic!("{what} failed");
        }
        self
    }
}

/// Connect to the docker daemon, honoring `DOCKER_HOST` and rootless
/// (`$XDG_RUNTIME_DIR/docker.sock`) setups before falling back to the system socket.
pub fn init_docker() -> Docker {
    // rootless docker uses a per-user socket; fall back to the system socket
    if let Ok(host) = std::env::var("DOCKER_HOST") {
        Docker::connect_with_socket(&host, 120, bollard::API_DEFAULT_VERSION).unwrap()
    } else if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        let path = format!("{xdg}/docker.sock");
        if std::path::Path::new(&path).exists() {
            Docker::connect_with_socket(&path, 120, bollard::API_DEFAULT_VERSION).unwrap()
        } else {
            Docker::connect_with_local_defaults().unwrap()
        }
    } else {
        Docker::connect_with_local_defaults().unwrap()
    }
}
