pub mod local;

pub use local::LocalBinary;

/// Abstraction over where and how a swarm instance is spawned.
#[allow(async_fn_in_trait)]
pub trait SwarmBackend {
    /// Spawn a swarm instance with the jsonnet config at `config_path` and return a handle
    /// with an already-connected zenoh session.
    async fn spawn(&self, config_path: &std::path::Path) -> super::process::SwarmProcess;
}
