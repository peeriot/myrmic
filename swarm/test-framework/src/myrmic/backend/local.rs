use std::sync::Arc;

use super::MyrmicBackend;

/// [`MyrmicBackend`] that runs programs on the host, on its own state directories or isolated
/// (see [`crate::myrmic::Myrmic::local_isolated`]).
#[derive(Clone, Default)]
pub struct Local {
    /// `None` for the host's own state directories; shared by the clones, so its directories live
    /// as long as any runtime or cell handle that may still need them for cleanup
    isolation: Option<Arc<Isolation>>,
}

/// A private myrmic setup on the host (see [`crate::myrmic::Myrmic::local_isolated`]): a
/// directory holding `data/` (`XDG_DATA_HOME`: runtime identities, databases, logs) and `run/`
/// (`XDG_RUNTIME_DIR`: the pid files `runtimes list` and `delete` read).
struct Isolation {
    dir: tempfile::TempDir,
}

impl Local {
    /// programs run with their own state directories
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
            isolation: Some(Arc::new(Isolation { dir })),
        }
    }
}

impl MyrmicBackend for Local {
    /// `program args`; in an isolation, with its state directories as `XDG_DATA_HOME` and
    /// `XDG_RUNTIME_DIR`.
    ///
    /// The variables are inherited, so they reach the CLI also when it is exec'd by `sh`.
    fn command(&self, program: &str, args: &[&str]) -> std::process::Command {
        let mut command = std::process::Command::new(program);
        command.args(args);
        if let Some(isolation) = &self.isolation {
            command
                .env("XDG_DATA_HOME", isolation.dir.path().join("data"))
                .env("XDG_RUNTIME_DIR", isolation.dir.path().join("run"));
        }
        command
    }

    fn wrapper_exit_codes(&self) -> &'static [i32] {
        // there is no wrapper: a program that cannot be started is a spawn error
        &[]
    }
}
