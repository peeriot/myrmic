use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};

use super::MyrmicBackend;

/// [`MyrmicBackend`] that runs programs on the host, isolated or not (see
/// [`crate::myrmic::Myrmic::local_isolated`]).
#[derive(Clone, Default)]
pub struct Local {
    /// shared by the clones, so its directories live as long as any runtime or cell handle that
    /// may still need them for cleanup
    isolation: Option<Arc<Isolation>>,
}

/// A private myrmic setup on the host (see [`crate::myrmic::Myrmic::local_isolated`]).
struct Isolation {
    /// holds `data/` (`XDG_DATA_HOME`: runtime identities, databases, logs) and `run/`
    /// (`XDG_RUNTIME_DIR`: the pid files `runtimes list` and `delete` read); shared with the
    /// isolations [`Local::cut_off`] creates
    dir: Arc<tempfile::TempDir>,
    /// the multicast group (`<address>:<port>`) its runtimes scout on, and the CLI and
    /// [`crate::myrmic::Myrmic::connect_session`] discover them by
    multicast_group: String,
}

impl Local {
    /// programs run with their own state directories and multicast group
    pub(crate) fn isolated() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("myrmic-isolated.")
            .tempdir()
            .expect("failed to create a temporary directory for an isolated myrmic");
        for subdir in ["data", "run"] {
            std::fs::create_dir(dir.path().join(subdir))
                .unwrap_or_else(|err| panic!("failed to create `{subdir}` in {dir:?}: {err}"));
        }
        Self {
            isolation: Some(Arc::new(Isolation {
                dir: Arc::new(dir),
                multicast_group: isolated_multicast_group(),
            })),
        }
    }

    /// this isolated backend's state directories, on a multicast group of its own
    pub(crate) fn cut_off(&self) -> Self {
        let isolation = self
            .isolation
            .as_ref()
            .expect("only an isolated shim has state directories to share");
        Self {
            isolation: Some(Arc::new(Isolation {
                dir: isolation.dir.clone(),
                multicast_group: isolated_multicast_group(),
            })),
        }
    }

    /// the isolation's multicast group; `None` for the host's default group
    pub(crate) fn multicast_group(&self) -> Option<&str> {
        self.isolation
            .as_ref()
            .map(|isolation| isolation.multicast_group.as_str())
    }
}

impl MyrmicBackend for Local {
    /// `program args`, inside the isolation, if any: its state directories as `XDG_DATA_HOME` and
    /// `XDG_RUNTIME_DIR`, and its multicast group as `DEFAULT_MYRMIC_MULTICAST_GROUP`, which the
    /// CLI takes like `--multicast-group`. The variables are inherited, so the CLI is inside the
    /// isolation also when it is exec'd by `sh`.
    fn command(&self, program: &str, args: &[&str]) -> std::process::Command {
        let mut command = std::process::Command::new(program);
        command.args(args);
        if let Some(isolation) = &self.isolation {
            command
                .env("XDG_DATA_HOME", isolation.dir.path().join("data"))
                .env("XDG_RUNTIME_DIR", isolation.dir.path().join("run"))
                .env("DEFAULT_MYRMIC_MULTICAST_GROUP", &isolation.multicast_group);
        }
        command
    }

    fn wrapper_exit_codes(&self) -> &'static [i32] {
        // there is no wrapper: a program that cannot be started is a spawn error
        &[]
    }

    /// in an isolation, the config that lets the runtime scout on the isolation's multicast group
    fn runtime_config(&self) -> Option<String> {
        self.isolation.as_ref().map(|isolation| {
            // JSON is YAML, which `runtimes start` parses
            serde_json::json!({
                "zenoh": { "scouting": { "multicast": { "address": isolation.multicast_group } } },
            })
            .to_string()
        })
    }
}

/// A multicast group no other isolation uses: the address from this process's pid
/// (administratively scoped 239.0.0.0/8; its low 24 bits cover every Linux pid), the port from a
/// per-process counter.
fn isolated_multicast_group() -> String {
    static NEXT_PORT: AtomicU16 = AtomicU16::new(7446);
    let port = NEXT_PORT.fetch_add(1, Ordering::Relaxed);
    assert!(
        port >= 7446,
        "ran out of ports for isolated multicast groups"
    );
    let pid = std::process::id();
    format!(
        "239.{}.{}.{}:{port}",
        (pid >> 16) & 0xFF,
        (pid >> 8) & 0xFF,
        pid & 0xFF
    )
}
