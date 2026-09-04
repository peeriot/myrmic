use std::path::PathBuf;

use super::SwarmBackend;
use crate::swarm::process::SwarmProcess;

/// [`SwarmBackend`] that spawns a swarm binary as a child process on the host,
/// listening on a freshly allocated localhost TCP port.
#[derive(Clone)]
pub struct LocalBinary {
    binary: PathBuf,
}

impl LocalBinary {
    /// wrap the swarm binary at `binary`
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
        }
    }
}

impl SwarmBackend for LocalBinary {
    async fn spawn(&self, config_path: &std::path::Path) -> SwarmProcess {
        // Allocate a free port (bind then drop releases it; tiny race window is acceptable for tests)
        let port = {
            let l =
                std::net::TcpListener::bind("127.0.0.1:0").expect("failed to bind ephemeral port");
            l.local_addr().unwrap().port()
        };

        // Generate a jsonnet wrapper that adds a TCP listen endpoint
        let abs_config = config_path
            .canonicalize()
            .expect("failed to canonicalize config path");
        let wrapper = format!(
            "local base = import \"{}\";\nbase + {{ zenoh+: {{ listen+: {{ endpoints+: {{ peer+: [\"tcp/127.0.0.1:{}\"] }} }} }} }}",
            abs_config.display(),
            port,
        );

        let temp_dir = tempfile::TempDir::new().expect("failed to create tempdir");
        let wrapper_path = temp_dir.path().join("swarm_wrapper.jsonnet");
        tokio::fs::write(&wrapper_path, wrapper)
            .await
            .expect("failed to write wrapper config");

        let child = tokio::process::Command::new(&self.binary)
            .arg(&wrapper_path)
            .kill_on_drop(true)
            .spawn()
            .expect("failed to spawn swarm binary");

        let endpoint = format!("tcp/127.0.0.1:{port}");
        let session = crate::swarm::open_client_session(&endpoint).await;

        SwarmProcess::new((child, temp_dir), session)
    }
}
