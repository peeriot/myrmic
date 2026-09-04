use crate::myrmic::cell::CellSpec;

pub mod docker;
pub mod local;
pub mod ssh;

pub(super) fn parse_runtime_list(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(runtime, _)| runtime.trim().to_owned())
        .collect()
}

pub(super) fn parse_status_lines(stdout: &str) -> Vec<String> {
    stdout.lines().map(|line| line.trim().to_owned()).collect()
}

/// Abstraction over where and how the myrmic CLI is executed (local binary or inside a
/// container). Each method maps to one myrmic CLI invocation.
// For now, let's see how far we get with async traits
#[allow(async_fn_in_trait)]
pub trait MyrmicBackend: Sized {
    /// run: myrmic runtimes start --name `name` -d [--tag `tag`...]
    async fn start_runtime(&self, name: &str, tags: &[&str]) {
        self.start_runtime_at(name, tags, None).await;
    }

    /// like [`Self::start_runtime`], but pointed at a `myrmic runtimes start <path>` config file
    /// already present on the target (`myrmic runtimes start -d --name name [--tag tag...]
    /// path`). Used to pin a runtime's zenoh listen endpoint to a fixed, predictable port —
    /// myrmic's default binds an ephemeral one — so a driver reaching the mesh from outside
    /// (e.g. an SSH-tunneled harness) has a stable port to point at.
    ///
    /// Backends with no notion of "a config file on the target" (local, docker — the harness's
    /// own client session already reaches them directly) ignore `config_path` and behave exactly
    /// like [`Self::start_runtime`].
    async fn start_runtime_at(&self, name: &str, tags: &[&str], config_path: Option<&str>) {
        let _ = config_path;
        self.start_runtime(name, tags).await;
    }

    /// run: myrmic runtimes delete `name`
    async fn delete_runtime(&self, name: &str);
    /// run: myrmic runtimes
    async fn list_runtimes(&self) -> Vec<String>;

    /// run: myrmic new
    async fn new_cell(&self, path: &std::path::Path, name: &str, sdk: Option<&str>);

    /// run: myrmic status
    async fn status(&self) -> Vec<String>;

    /// run: myrmic send `sri` `command`
    async fn send(&self, sri: &str, command: &str) -> Option<String>;

    /// run: myrmic deploy --sri `sri`
    async fn deploy(&self, cell: CellSpec, sri: &str, tags: &[&str]);

    /// run: myrmic deploy `app-spec.yml`
    async fn deploy_app(&self, app_spec: &std::path::Path);

    /// run: myrmic delete
    async fn delete_cell(&self, sri: &str);

    /// Blocking best-effort variant of [`Self::delete_runtime`] for use in `Drop`.
    fn delete_runtime_blocking(&self, name: &str) -> Result<(), String>;

    /// Blocking best-effort variant of [`Self::delete_cell`] for use in `Drop`.
    fn delete_cell_blocking(&self, sri: &str) -> Result<(), String>;

    /// Backend-specific cleanup (e.g. stopping the container); no-op by default.
    async fn cleanup(&self) {}
}
