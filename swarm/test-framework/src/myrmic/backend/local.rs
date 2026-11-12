use std::sync::Arc;

use super::MyrmicBackend;

/// [`MyrmicBackend`] that runs programs on the test's machine, isolated: on state directories of
/// their own (see [`crate::host::Local::myrmic_isolated`]).
#[derive(Clone)]
pub struct Local {
    /// Holds `data/` (`XDG_DATA_HOME`: runtime identities, databases, logs) and `run/`
    /// (`XDG_RUNTIME_DIR`: the pid files `runtimes list` and `delete` read). Shared by the clones,
    /// so it lives as long as any runtime or cell handle that may still need it for cleanup.
    dir: Arc<tempfile::TempDir>,
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
        Self { dir: Arc::new(dir) }
    }
}

impl MyrmicBackend for Local {
    /// `program args`, with the isolation's state directories as `XDG_DATA_HOME` and
    /// `XDG_RUNTIME_DIR`.
    ///
    /// The variables are inherited, so they reach the CLI also when it is exec'd by `sh`.
    fn command(&self, program: &str, args: &[&str]) -> std::process::Command {
        let mut command = std::process::Command::new(program);
        command
            .args(args)
            .env("XDG_DATA_HOME", self.dir.path().join("data"))
            .env("XDG_RUNTIME_DIR", self.dir.path().join("run"));
        command
    }

    fn wrapper_exit_codes(&self) -> &'static [i32] {
        // there is no wrapper: a program that cannot be started is a spawn error
        &[]
    }
}
